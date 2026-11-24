//! Keeping the index current: Graph delta / Gmail history per mailbox, run
//! before reads. A bootstrap can take a while on a big mailbox, so it runs
//! in the background and a read waits only up to its budget, answering from
//! what is there and saying that older mail is still loading.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use pidge_core::provider::{Provider, ProviderError};
use pidge_store::SyncState;

use crate::classify::sort_new;
use crate::ctx::{Frontend, User};

/// A mailbox synced this recently is not asked again.
const FRESH: chrono::Duration = chrono::Duration::seconds(15);
/// A read is answered from an index this fresh without waiting.
const RECENT: chrono::Duration = chrono::Duration::seconds(90);
/// Otherwise it waits this long before it is answered anyway.
const STALE_BUDGET: Duration = Duration::from_secs(3);
/// How often an active user's index is brought up to date, and an idle one's.
const ACTIVE: Duration = Duration::from_secs(60);
const IDLE_ROUNDS: u32 = 10;

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
        // A message that only moved leaves one folder and shows up in another.
        let gone: Vec<String> = page
            .removed
            .iter()
            .filter(|id| !page.upserts.iter().any(|u| u.id == **id))
            .cloned()
            .collect();
        user.store
            .remove_items(&account, &gone)
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

/// Bring the given mailboxes up to date and sort what is new. A read of an
/// index that is recent does not wait; one that is older waits a moment; a
/// first load waits up to `budget`.
pub async fn ensure(
    user: &Arc<User>,
    providers: &[Arc<dyn Provider>],
    budget: Duration,
) -> SyncReport {
    let now = Utc::now();
    let server = user.frontend() == Frontend::Mcp;
    let start = tokio::time::Instant::now();
    let mut handles = Vec::new();
    for p in providers {
        let account = p.address().to_string();
        let state = user.store.sync_state(&account).ok();
        let loaded = state.as_ref().is_some_and(|s| s.complete);
        let recent = state
            .and_then(|s| s.synced_at)
            .is_some_and(|t| now - t < RECENT);
        let wait = match (server && loaded, recent) {
            (true, true) => Duration::ZERO,
            (true, false) => STALE_BUDGET,
            (false, _) => budget,
        };
        let task = tokio::spawn(sync_account(user.clone(), p.clone()));
        handles.push((account, loaded, start + wait, task));
    }
    let mut report = SyncReport::default();
    for (account, loaded, deadline, task) in handles {
        match tokio::time::timeout_at(deadline, task).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(ProviderError::Auth { .. }))) => report.failed.push((
                account,
                "needs to sign in again (settings action=connect)".into(),
            )),
            Ok(Ok(Err(e))) => report
                .failed
                .push((account, format!("could not sync: {e}"))),
            Ok(Err(_)) => report.failed.push((account, "sync stopped".into())),
            // Still running: it carries on in the background.
            Err(_) if loaded => {}
            Err(_) => report.loading.push(account),
        }
    }
    sort_new(user).await;
    if server {
        keep(user);
    }
    report
}

/// Keep the user's index current while they are active, and (on a server
/// that is always on) now and then while they are not.
pub fn keep(user: &Arc<User>) {
    let mut slot = user.keeper.lock().unwrap_or_else(|p| p.into_inner());
    if slot.as_ref().is_some_and(|t| !t.is_finished()) {
        return;
    }
    let weak = Arc::downgrade(user);
    *slot = Some(tokio::spawn(async move {
        let mut round = 0_u32;
        loop {
            let Some(user) = weak.upgrade() else { return };
            round = round.wrapping_add(1);
            let active = user.is_active();
            if !active && !user.host.always_on() {
                return;
            }
            if round % IDLE_ROUNDS == 1 {
                let _ = user.store.housekeeping();
            }
            if active || round % IDLE_ROUNDS == 1 {
                refresh(&user).await;
            }
            drop(user);
            tokio::time::sleep(ACTIVE).await;
        }
    }));
}

/// One round in the background: mail, sorting, the calendar.
pub async fn refresh(user: &Arc<User>) {
    let providers = user.providers();
    let syncs = providers
        .iter()
        .map(|p| sync_account(user.clone(), p.clone()));
    for r in futures::future::join_all(syncs).await {
        if let Err(e) = r {
            tracing::debug!(error = %e, "background sync failed");
        }
    }
    sort_new(user).await;
    crate::happenings::refresh_calendar(user, &providers).await;
}

/// The default budget: a CLI can wait for a full bootstrap, a voice
/// harness should not.
pub fn budget(user: &User) -> Duration {
    match user.frontend() {
        crate::ctx::Frontend::Cli => Duration::from_secs(120),
        crate::ctx::Frontend::Mcp => Duration::from_secs(8),
    }
}
