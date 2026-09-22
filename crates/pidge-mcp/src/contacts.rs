//! Per-user contact caches for recipient-name resolution (spec §1.6).
//!
//! In memory only, one [`ContactsCache`] per signed-in user, built lazily
//! from every mailbox the user owns (Outlook's ranked people list plus the
//! senders of recent inbox mail) and rebuilt once it is a day old.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use pidge_core::{ContactSource, ContactsCache};
use rmcp::ErrorData as McpError;

use crate::state::SharedState;
use crate::users::{UserRecord, user_hash};

/// How long a built cache is used before it is rebuilt.
const TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// People asked of Graph's ranked list, per mailbox.
const PEOPLE_TOP: usize = 200;
/// Recent inbox rows whose senders are added, per mailbox.
const INBOX_ROWS: usize = 100;

/// Sign-in address → (built at, contacts).
#[derive(Default)]
pub struct ContactCaches {
    inner: Mutex<HashMap<String, (Instant, ContactsCache)>>,
}

impl ContactCaches {
    /// `user`'s contacts, built on first use and again once a day old.
    /// Building is best-effort: a mailbox Graph won't serve is skipped, so
    /// this never fails; the `Result` leaves room for a store-backed cache.
    pub async fn get(
        &self,
        state: &SharedState,
        user: &UserRecord,
    ) -> Result<ContactsCache, McpError> {
        if let Some(cache) = self.fresh(&user.signin) {
            return Ok(cache);
        }
        let cache = build(state, user).await;
        self.inner
            .lock()
            .expect("contacts lock")
            .insert(user.signin.clone(), (Instant::now(), cache.clone()));
        Ok(cache)
    }

    fn fresh(&self, signin: &str) -> Option<ContactsCache> {
        let map = self.inner.lock().expect("contacts lock");
        map.get(signin)
            .filter(|(built, _)| built.elapsed() < TTL)
            .map(|(_, cache)| cache.clone())
    }
}

/// Every owned mailbox's people (in Graph's relevance order) and recent
/// inbox senders, merged the way `pidge contacts refresh` merges them.
async fn build(state: &SharedState, user: &UserRecord) -> ContactsCache {
    let now = Utc::now();
    let mut cache = ContactsCache::default();
    let mut skipped = 0;
    for account in &user.mailboxes {
        match state.graph.list_people(account, PEOPLE_TOP).await {
            Ok(people) => {
                for (rank, p) in people.iter().enumerate() {
                    // People carry no timestamp; stepping back one second per
                    // rank keeps Graph's relevance order in ambiguity lists.
                    let seen = now - chrono::Duration::seconds(rank as i64);
                    cache.upsert(&p.address, &p.display_name, seen, ContactSource::Mail);
                }
            }
            Err(_) => skipped += 1,
        }
        match state
            .graph
            .list_folder(account, "inbox", INBOX_ROWS, 0, false)
            .await
        {
            Ok(page) => {
                for m in page.messages {
                    cache.upsert(
                        &m.from.address,
                        &m.from.name,
                        m.received_at,
                        ContactSource::Mail,
                    );
                }
            }
            Err(_) => skipped += 1,
        }
    }
    cache.mark_refreshed(now);
    tracing::info!(
        user = %user_hash(&user.signin),
        contacts = cache.by_email.len(),
        skipped_lookups = skipped,
        "built contact cache"
    );
    cache
}

#[cfg(test)]
mod tests {
    use pidge_core::contacts::ResolveOutcome;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, ResponseTemplate};

    use crate::tools::tests::{ToolHarness, access_token};

    const JANE: &str = "jane@example.com";
    const WORK: &str = "work@example.com";

    fn bearer(mailbox: &str) -> String {
        format!("Bearer {}", access_token(mailbox))
    }

    #[tokio::test]
    async fn built_once_from_people_and_inbox_senders_skipping_a_failing_mailbox() {
        let h = ToolHarness::new(&[JANE, WORK]).await;
        Mock::given(method("GET"))
            .and(path("/v1.0/me/people"))
            .and(query_param("$top", "200"))
            .and(header("authorization", bearer(JANE).as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                { "displayName": "Bob Builder",
                  "scoredEmailAddresses": [{ "address": "bob@example.com" }] },
            ]})))
            .expect(1)
            .mount(&h.graph)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1.0/me/mailFolders/inbox/messages"))
            .and(query_param("$top", "100"))
            .and(header("authorization", bearer(JANE).as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [{
                "id": "M1",
                "from": { "emailAddress": { "name": "Carol Sender", "address": "carol@example.com" } },
                "receivedDateTime": "2026-09-23T08:00:00Z",
            }]})))
            .expect(1)
            .mount(&h.graph)
            .await;
        // WORK is unavailable: skipped, not fatal.
        Mock::given(method("GET"))
            .and(header("authorization", bearer(WORK).as_str()))
            .respond_with(ResponseTemplate::new(500))
            .mount(&h.graph)
            .await;

        let record = h.record().await;
        let cache = h.state.contacts.get(&h.state, &record).await.unwrap();
        assert_eq!(
            cache.resolve_any("bob"),
            ResolveOutcome::One("bob@example.com".into())
        );
        assert_eq!(
            cache.resolve_any("Carol"),
            ResolveOutcome::One("carol@example.com".into())
        );

        // Within the TTL the second call is served from memory (expect(1) above).
        let again = h.state.contacts.get(&h.state, &record).await.unwrap();
        assert_eq!(again.by_email.len(), 2);
    }
}
