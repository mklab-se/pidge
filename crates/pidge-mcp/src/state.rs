//! Shared, process-wide state handed to every handler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use pidge_client::GraphClient;

use crate::cache::ReadCache;
use crate::config::Config;
use crate::mailbox::SecretTokenBackend;
use crate::oauth::jwt::Signer;
use crate::secrets::SharedSecrets;
use crate::users::UserStore;

/// The read cache's TTL and per-user LRU bound (spec §1.9).
const CACHE_TTL: StdDuration = StdDuration::from_secs(60);
const CACHE_PER_USER: usize = 256;

/// What a Microsoft sign-in is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingKind {
    /// An MCP client's OAuth flow: the Microsoft account becomes the
    /// signed-in user (and their first mailbox).
    SignIn,
    /// An `accounts_connect` link: the Microsoft account becomes an
    /// additional mailbox owned by `owner` (a sign-in address).
    Connect { owner: String },
}

/// An authorization the user has started but not finished at Microsoft.
/// Keyed by the `state` we send to Microsoft; lives in memory for minutes.
/// The client fields are empty for [`PendingKind::Connect`], where no OAuth
/// client is waiting.
#[derive(Debug, Clone)]
pub struct PendingAuthorization {
    pub kind: PendingKind,
    pub client_id: String,
    pub client_redirect_uri: String,
    pub client_state: Option<String>,
    pub code_challenge: String,
    pub microsoft_verifier: String,
    /// For [`PendingKind::Connect`]: the nonce set as a cookie by the
    /// confirmation page, which the Continue step must present, so the
    /// Microsoft redirect only happens in a browser that saw the owner.
    /// `None` until that page is shown, and always for sign-ins.
    pub consent_nonce: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub const PENDING_TTL: Duration = Duration::minutes(10);

pub struct AppState {
    pub config: Config,
    pub signer: Signer,
    pub graph: GraphClient,
    /// The token backend `graph` uses; held here so a disconnect or a fresh
    /// connect can evict its cached tokens.
    pub token_backend: Arc<SecretTokenBackend>,
    pub users: UserStore,
    /// Per-user, 60 s, LRU-bounded cache for read tools; see [`crate::cache`].
    pub cache: ReadCache,
    pending: Mutex<HashMap<String, PendingAuthorization>>,
    /// `jti` → expiry of authorization codes already redeemed, so a code
    /// can't be replayed inside its two-minute lifetime.
    used_codes: Mutex<HashMap<String, i64>>,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    /// `token_backend` must be the backend `graph`'s `AuthClient` was built with.
    pub fn new(
        config: Config,
        signer: Signer,
        graph: GraphClient,
        token_backend: Arc<SecretTokenBackend>,
        secrets: SharedSecrets,
    ) -> Self {
        Self {
            config,
            signer,
            graph,
            token_backend,
            users: UserStore::new(secrets),
            cache: ReadCache::new(CACHE_TTL, CACHE_PER_USER),
            pending: Mutex::new(HashMap::new()),
            used_codes: Mutex::new(HashMap::new()),
        }
    }

    pub fn insert_pending(&self, state: String, pending: PendingAuthorization) {
        let mut map = self.pending.lock().expect("pending lock");
        let cutoff = Utc::now() - PENDING_TTL;
        map.retain(|_, p| p.created_at > cutoff);
        map.insert(state, pending);
    }

    /// Returns a copy of a live pending authorization without consuming it,
    /// so a connect link can be opened (and retried) before the callback.
    pub fn peek_pending(&self, state: &str) -> Option<PendingAuthorization> {
        let map = self.pending.lock().expect("pending lock");
        map.get(state)
            .filter(|p| p.created_at > Utc::now() - PENDING_TTL)
            .cloned()
    }

    /// Records the confirmation page's cookie nonce on a live pending entry;
    /// replaces any earlier one. No-op if the entry is gone or expired.
    pub fn set_consent_nonce(&self, state: &str, nonce: String) {
        let mut map = self.pending.lock().expect("pending lock");
        if let Some(p) = map
            .get_mut(state)
            .filter(|p| p.created_at > Utc::now() - PENDING_TTL)
        {
            p.consent_nonce = Some(nonce);
        }
    }

    /// Removes and returns the pending authorization, so a Microsoft
    /// callback can only be consumed once.
    pub fn take_pending(&self, state: &str) -> Option<PendingAuthorization> {
        let mut map = self.pending.lock().expect("pending lock");
        let pending = map.remove(state)?;
        (pending.created_at > Utc::now() - PENDING_TTL).then_some(pending)
    }

    /// Returns `false` if this code id was already redeemed.
    pub fn mark_code_used(&self, jti: &str, exp: i64) -> bool {
        let mut map = self.used_codes.lock().expect("used codes lock");
        let now = Utc::now().timestamp();
        map.retain(|_, e| *e > now);
        map.insert(jti.to_string(), exp).is_none()
    }
}
