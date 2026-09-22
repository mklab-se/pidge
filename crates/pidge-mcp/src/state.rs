//! Shared, process-wide state handed to every handler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use pidge_client::GraphClient;

use crate::config::Config;
use crate::oauth::jwt::Signer;
use crate::secrets::SharedSecrets;
use crate::users::UserStore;

/// An authorization the user has started but not finished at Microsoft.
/// Keyed by the `state` we send to Microsoft; lives in memory for minutes.
#[derive(Debug, Clone)]
pub struct PendingAuthorization {
    pub client_id: String,
    pub client_redirect_uri: String,
    pub client_state: Option<String>,
    pub code_challenge: String,
    pub microsoft_verifier: String,
    pub created_at: DateTime<Utc>,
}

const PENDING_TTL: Duration = Duration::minutes(10);

pub struct AppState {
    pub config: Config,
    pub signer: Signer,
    pub graph: GraphClient,
    #[allow(dead_code)] // kept for the upcoming connect_mailbox tool
    pub secrets: SharedSecrets,
    #[allow(dead_code)] // wired into the sign-in callback and tools in Task 8+
    pub users: UserStore,
    pending: Mutex<HashMap<String, PendingAuthorization>>,
    /// `jti` → expiry of authorization codes already redeemed, so a code
    /// can't be replayed inside its two-minute lifetime.
    used_codes: Mutex<HashMap<String, i64>>,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config, signer: Signer, graph: GraphClient, secrets: SharedSecrets) -> Self {
        Self {
            config,
            signer,
            graph,
            users: UserStore::new(secrets.clone()),
            secrets,
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
