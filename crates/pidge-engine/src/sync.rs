//! Keeping the index current: Graph delta / Gmail history per mailbox, run
//! before reads. A bootstrap can take a while on a big mailbox, so it runs
//! in the background and a read waits only up to its budget, answering from
//! what is there and saying that older mail is still loading.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use pidge_core::provider::{Provider, ProviderError};
use pidge_store::SyncState;

use crate::classify::classify_pending;
use crate::ctx::User;

/// A mailbox synced this recently is not asked again.
const FRESH: chrono::Duration = chrono::Duration::seconds(15);
/// Time the AI may take on new mail before an answer goes out anyway.
const CLASSIFY_BUDGET: Duration = Duration::from_secs(4);

/// What a read should mention about the index.
#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    /// Mailboxes whose first load has not finished.
    pub loading: Vec<String>,
    /// Mailboxes that failed, with why.
    pub failed: Vec<(String, String)>,
}

impl SyncReport {
    /// Lines for the top of an answer.
    pub fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.loading.is_empty() {
            out.push(format!(
                "Still loading older mail for {}; counts may grow.",
                self.loading.join(", ")
            ));
        }
        for (account, why) in &self.failed {
            out.push(format!("{account}: {why}"));
        }
        out
    }
}

async fn sync_account(user: Arc<User>, provider: Arc<dyn Provider>) -> Result<(), ProviderError> {
    let account = provider.address().to_string();
    let lock = user.sync_lock(&account);
    let _guard = lock.lock().await;
    let mut st = user
        .store
        .sync_state(&account)
        .map_err(|e| ProviderError::Other(e.to_string()))?;
    let now = Utc::now();
    if st.complete && st.synced_at.is_some_and(|t| now - t < FRESH) {
        return Ok(());
    }
    let prefs = user.prefs().await;
    let window_start = now - chrono::Duration::days(prefs.window_days());
    let mut retried = false;
    loop {
        let page = match provider.sync_mail(window_start, st.cursor.as_deref()).await {
            Ok(p) => p,
            Err(ProviderError::CursorExpired) if !retried => {
                retried = true;
                st = SyncState {
                    account: account.clone(),
                    cursor: None,
                    synced_at: None,
                    window_start: None,
                    complete: false,
                };
                continue;
            }
            Err(e) => return Err(e),
        };
        let store_err = |e: pidge_store::StoreError| ProviderError::Other(e.to_string());
        user.store
            .remove_items(&account, &page.removed)
            .map_err(store_err)?;
        user.store.upsert_items(&page.upserts).map_err(store_err)?;
        st.cursor = Some(page.cursor);
        st.window_start = Some(window_start);
        if !page.more {
            st.complete = true;
            st.synced_at = Some(Utc::now());
            user.store.save_sync(&st).map_err(store_err)?;
            break;
        }
        user.store.save_sync(&st).map_err(store_err)?;
    }
    let _ = user.store.prune_before(window_start);
    refresh_people(&user, provider.as_ref()).await;
    Ok(())
}

/// Provider people hints, at most once a day.
async fn refresh_people(user: &User, provider: &dyn Provider) {
    let key = format!("people:{}", provider.address());
    if user
        .store
        .get_doc::<bool>("refreshed", &key)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    if let Ok(people) = provider.people().await
        && !people.is_empty()
    {
        let hints: Vec<(String, String)> =
            people.into_iter().map(|p| (p.name, p.address)).collect();
        let _ = user.store.set_people_hints(&hints);
    }
    let _ = user.store.put_doc(
        "refreshed",
        &key,
        &true,
        Some(Utc::now() + chrono::Duration::hours(24)),
    );
}

/// Bring the given mailboxes up to date, waiting at most `budget`, then
/// classify what is new.
pub async fn ensure(
    user: &Arc<User>,
    providers: &[Arc<dyn Provider>],
    budget: Duration,
) -> SyncReport {
    let mut handles = Vec::new();
    for p in providers {
        let (u, p) = (user.clone(), p.clone());
        let account = p.address().to_string();
        handles.push((account, tokio::spawn(sync_account(u, p))));
    }
    let deadline = tokio::time::Instant::now() + budget;
    let mut report = SyncReport::default();
    for (account, handle) in handles {
        match tokio::time::timeout_at(deadline, handle).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(ProviderError::Auth { .. }))) => report.failed.push((
                account,
                "needs to sign in again (accounts action=connect)".into(),
            )),
            Ok(Ok(Err(e))) => report
                .failed
                .push((account, format!("could not sync: {e}"))),
            Ok(Err(_)) => report.failed.push((account, "sync stopped".into())),
            // Still running: it carries on in the background.
            Err(_) => report.loading.push(account),
        }
    }
    classify_pending(user, CLASSIFY_BUDGET).await;
    report
}

/// The default budget: a CLI can wait for a full bootstrap, a voice
/// harness should not.
pub fn budget(user: &User) -> Duration {
    match user.frontend() {
        crate::ctx::Frontend::Cli => Duration::from_secs(120),
        crate::ctx::Frontend::Mcp => Duration::from_secs(8),
    }
}
