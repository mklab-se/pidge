//! The bridge to `pidge-engine`: one engine [`User`] per signed-in user,
//! built from their user record and mailbox secrets, kept warm in memory
//! (so background work, held sends and the index survive between calls).
//!
//! Ownership is checked here, once per refresh: a mailbox is handed to the
//! engine only if the user's record lists it *and* the mailbox's own record
//! names the user as owner. The engine never sees anything else, so no
//! operation can reach another user's mailbox.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pidge_client::GraphProvider;
use pidge_core::Attachment;
use pidge_core::provider::{Provider, ProviderKind};
use pidge_engine::mcp::UserSource;
use pidge_engine::prefs::{Category, Example, Prefs, PrefsStore};
use pidge_engine::{AccountInfo, ErrorKind, FilePrefs, Frontend, Host, OpError, OpResult, User};
use pidge_store::Store;
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use serde_json::Value;

use crate::config::PrefsBackend;
use crate::oauth::bearer::AuthenticatedUser;
use crate::oauth::jwt::random_id;
use crate::state::{AppState, PENDING_TTL, PendingAuthorization, PendingKind, SharedState};
use crate::users::{UserRecord, log_store_error, user_hash, user_hash_long};

/// How long a user's mailbox list is trusted before it is read again.
const REFRESH: Duration = Duration::from_secs(120);

struct Entry {
    user: Arc<User>,
    refreshed: Instant,
}

/// Every signed-in user's engine.
#[derive(Default)]
pub struct Engines {
    users: Mutex<HashMap<String, Entry>>,
}

impl Engines {
    /// Re-read the user's mailboxes and AI key on their next call (after a
    /// connect, a disconnect or an AI key change).
    pub fn forget(&self, signin: &str) {
        if let Some(e) = self
            .users
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(&signin.to_ascii_lowercase())
        {
            e.refreshed = Instant::now() - REFRESH * 2;
        }
    }

    /// The engine user for `signin`.
    pub async fn user(&self, state: &SharedState, signin: &str) -> Result<Arc<User>> {
        let key = signin.to_ascii_lowercase();
        let existing = {
            let map = self.users.lock().unwrap_or_else(|p| p.into_inner());
            map.get(&key)
                .map(|e| (e.user.clone(), e.refreshed.elapsed() < REFRESH))
        };
        if let Some((user, true)) = existing {
            return Ok(user);
        }
        let record = load_record(state, &key).await?;
        let providers = providers(state, &record).await?;
        let ai = ai_client(state, &key).await;
        let user = match existing {
            Some((user, _)) => {
                user.set_providers(providers);
                user
            }
            None => {
                let cfg = &state.config.engine;
                std::fs::create_dir_all(&cfg.index_dir).ok();
                let store = Arc::new(
                    Store::open(&cfg.index_dir.join(format!("{}.db", user_hash_long(&key))))
                        .context("opening the index")?,
                );
                let prefs: Arc<dyn PrefsStore> = match &cfg.prefs {
                    PrefsBackend::Dir(dir) => Arc::new(FilePrefs::new(
                        dir.join(format!("{}.json", user_hash_long(&key))),
                    )),
                    PrefsBackend::Blob(url) => Arc::new(BlobPrefs::new(url.clone(), &key)?),
                };
                seed_prefs(prefs.as_ref(), &record).await;
                let host = Arc::new(HostedHost {
                    state: Arc::downgrade(state),
                    signin: key.clone(),
                });
                User::new(user_hash(&key), store, prefs, providers, host)
            }
        };
        user.set_ai(ai.map(Arc::new));
        self.users.lock().unwrap_or_else(|p| p.into_inner()).insert(
            key,
            Entry {
                user: user.clone(),
                refreshed: Instant::now(),
            },
        );
        Ok(user)
    }
}

async fn load_record(state: &AppState, signin: &str) -> Result<UserRecord> {
    match state.users.load(signin).await? {
        Some(r) => Ok(r),
        None => {
            let r = UserRecord::new(signin);
            state.users.save(&r).await?;
            Ok(r)
        }
    }
}

/// The user's mailboxes the engine may use: listed on the record *and*
/// owned by the user per the mailbox's own record.
async fn providers(state: &SharedState, record: &UserRecord) -> Result<Vec<Arc<dyn Provider>>> {
    let mut out: Vec<Arc<dyn Provider>> = Vec::new();
    for mailbox in &record.mailboxes {
        let rec = state.users.load_mailbox(mailbox).await?;
        let provider = match rec {
            Some(r) if !r.owner.eq_ignore_ascii_case(&record.signin) => {
                tracing::warn!(
                    user = %user_hash(&record.signin),
                    mailbox = %user_hash(mailbox),
                    "mailbox left out: its record names another owner"
                );
                continue;
            }
            Some(r) => r.provider,
            // Nothing stored: Microsoft, which then reports it needs a reconnect.
            None => ProviderKind::Microsoft,
        };
        match provider {
            ProviderKind::Microsoft => {
                out.push(Arc::new(GraphProvider::new(
                    state.graph.clone(),
                    mailbox.clone(),
                )));
            }
            ProviderKind::Google => {
                let Some((id, secret)) = &state.config.engine.google else {
                    tracing::warn!(mailbox = %user_hash(mailbox), "Gmail mailbox skipped: no Google client configured");
                    continue;
                };
                let auth = pidge_gmail::GoogleAuth::new(
                    state.http.clone(),
                    pidge_gmail::GoogleOAuthConfig::new(id.clone(), secret.clone()),
                    state.token_backend.clone(),
                    mailbox.clone(),
                );
                out.push(Arc::new(pidge_gmail::GmailProvider::new(auth)));
            }
        }
    }
    Ok(out)
}

async fn ai_client(state: &AppState, signin: &str) -> Option<ailloy::Client> {
    match crate::ai::load(&state.secrets, signin).await {
        Ok(Some(secret)) => crate::ai::build_client(&secret, state.ai_endpoint.as_deref()).ok(),
        _ => None,
    }
}

/// First use: carry the user's settings and their inbox-review categories
/// over from the user record.
async fn seed_prefs(store: &dyn PrefsStore, record: &UserRecord) {
    match store.load().await {
        Ok(None) => {}
        _ => return,
    }
    let mut p = Prefs {
        timezone: Some(record.timezone.clone()).filter(|t| !t.is_empty()),
        default_sender: Some(record.default_sender.clone()).filter(|s| !s.is_empty()),
        vips: record.trusted_senders.clone(),
        ..Default::default()
    };
    if let Some(cats) = record
        .review
        .as_ref()
        .and_then(|r| r.get("categories"))
        .and_then(Value::as_array)
    {
        let defaults = pidge_engine::prefs::default_categories();
        let converted: Vec<Category> = cats
            .iter()
            .filter_map(|c| {
                let name = c.get("name")?.as_str()?.to_string();
                let base = defaults.iter().find(|d| d.name == name);
                Some(Category {
                    description: c
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    rules: c
                        .get("rules")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    examples: c
                        .get("examples")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|e| {
                                    Some(Example {
                                        from: e.get("from")?.as_str()?.to_string(),
                                        subject: e.get("subject")?.as_str()?.to_string(),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    kinds: base.map(|b| b.kinds.clone()).unwrap_or_default(),
                    sweep: c
                        .get("bulk_suggestion")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        || base.is_some_and(|b| b.sweep),
                    name,
                })
            })
            .collect();
        if !converted.is_empty() {
            p.categories = Some(converted);
            p.categories_confirmed = true;
        }
    }
    let _ = store.save(&p).await;
}

/// Preferences in an Azure Blob container, one blob per user, written with
/// the server's managed identity.
pub struct BlobPrefs {
    url: url::Url,
    credential: Arc<dyn azure_core::credentials::TokenCredential>,
    http: reqwest::Client,
}

impl BlobPrefs {
    pub fn new(container: url::Url, signin: &str) -> Result<Self> {
        let url: url::Url = format!(
            "{}/{}.json",
            container.as_str().trim_end_matches('/'),
            user_hash_long(signin)
        )
        .parse()
        .context("blob url")?;
        Ok(Self {
            url,
            credential: crate::secrets::azure_credential()?,
            http: reqwest::Client::new(),
        })
    }

    async fn token(&self) -> Result<String, String> {
        self.credential
            .get_token(&["https://storage.azure.com/.default"], None)
            .await
            .map(|t| t.token.secret().to_string())
            .map_err(|e| format!("storage token: {e}"))
    }
}

#[async_trait]
impl PrefsStore for BlobPrefs {
    async fn load(&self) -> Result<Option<Prefs>, String> {
        let resp = self
            .http
            .get(self.url.clone())
            .bearer_auth(self.token().await?)
            .header("x-ms-version", "2021-08-06")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        match resp.status().as_u16() {
            404 => Ok(None),
            200 => {
                let text = resp.text().await.map_err(|e| e.to_string())?;
                serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|e| e.to_string())
            }
            s => Err(format!("preferences store answered {s}")),
        }
    }

    async fn save(&self, prefs: &Prefs) -> Result<(), String> {
        let body = serde_json::to_vec(prefs).map_err(|e| e.to_string())?;
        let resp = self
            .http
            .put(self.url.clone())
            .bearer_auth(self.token().await?)
            .header("x-ms-version", "2021-08-06")
            .header("x-ms-blob-type", "BlockBlob")
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "preferences store answered {}",
                resp.status().as_u16()
            ))
        }
    }
}

/// What the hosted frontend does for one user.
pub struct HostedHost {
    state: Weak<AppState>,
    signin: String,
}

impl HostedHost {
    fn state(&self) -> OpResult<SharedState> {
        self.state
            .upgrade()
            .ok_or_else(|| OpError::internal("the server is shutting down"))
    }
}

fn store_failure(what: &str) -> OpError {
    OpError::internal(format!("could not {what}; try again shortly"))
}

#[async_trait]
impl Host for HostedHost {
    fn frontend(&self) -> Frontend {
        Frontend::Mcp
    }

    fn default_timezone(&self) -> String {
        "Europe/Stockholm".into()
    }

    async fn accounts(&self) -> Vec<AccountInfo> {
        let Ok(state) = self.state() else {
            return Vec::new();
        };
        let Ok(Some(record)) = state.users.load(&self.signin).await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for mailbox in &record.mailboxes {
            let rec = state.users.load_mailbox(mailbox).await.ok().flatten();
            let (provider, status) = match rec {
                Some(r) if !r.owner.eq_ignore_ascii_case(&record.signin) => {
                    (r.provider, "needs reconnect".to_string())
                }
                Some(r) => {
                    let ok = !r.tokens.needs_refresh()
                        || match r.provider {
                            ProviderKind::Microsoft => {
                                state.graph.auth().get_valid_token(mailbox).await.is_ok()
                            }
                            ProviderKind::Google => true,
                        };
                    (
                        r.provider,
                        if ok { "ok" } else { "needs reconnect" }.to_string(),
                    )
                }
                None => (ProviderKind::Microsoft, "needs reconnect".into()),
            };
            let status = if *mailbox == record.signin {
                format!("{status} (sign-in)")
            } else {
                status
            };
            out.push(AccountInfo {
                address: mailbox.clone(),
                provider,
                status,
            });
        }
        out
    }

    async fn connect(&self, provider: ProviderKind, email: Option<&str>) -> OpResult<String> {
        let state = self.state()?;
        if provider == ProviderKind::Google && state.config.engine.google.is_none() {
            return Err(OpError::new(
                ErrorKind::Unsupported,
                "this server has no Google client configured, so Gmail cannot be connected here",
            ));
        }
        let requested = email
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_ascii_lowercase);
        let key = random_id();
        state.insert_pending(
            key.clone(),
            PendingAuthorization {
                kind: PendingKind::Connect {
                    owner: self.signin.clone(),
                    mailbox: requested.clone(),
                    provider,
                },
                client_id: String::new(),
                client_redirect_uri: String::new(),
                client_state: None,
                code_challenge: String::new(),
                microsoft_verifier: random_id() + &random_id(),
                consent_nonce: None,
                callback_nonce: None,
                created_at: Utc::now(),
            },
        );
        tracing::info!(user = %user_hash(&self.signin), provider = provider.as_str(), "connect link issued");
        let target = requested.as_deref().unwrap_or("the mailbox");
        let at = match provider {
            ProviderKind::Microsoft => "Microsoft",
            ProviderKind::Google => "Google",
        };
        Ok(format!(
            "Connect {target} by opening: {}/connect?state={key}  (valid {} minutes)\n\
             The user signs in at {at} with that mailbox's account.\n\
             next: accounts once they have finished",
            state.config.base_url(),
            PENDING_TTL.num_minutes()
        ))
    }

    async fn disconnect(&self, address: &str) -> OpResult<String> {
        let state = self.state()?;
        let mut record = state
            .users
            .load(&self.signin)
            .await
            .map_err(|_| store_failure("load your account"))?
            .ok_or_else(|| store_failure("load your account"))?;
        let mailbox = record
            .mailboxes
            .iter()
            .find(|m| m.eq_ignore_ascii_case(address))
            .cloned()
            .ok_or_else(|| OpError::not_found(format!("{address} is not one of your mailboxes")))?;
        if mailbox == record.signin {
            return Err(OpError::usage(format!(
                "{mailbox} is the sign-in mailbox and cannot be disconnected"
            )));
        }
        record.mailboxes.retain(|m| *m != mailbox);
        if record.default_sender == mailbox {
            record.default_sender = record.signin.clone();
        }
        state
            .users
            .save(&record)
            .await
            .map_err(|_| store_failure("save your account"))?;
        if let Err(e) = state.users.delete_mailbox(&mailbox).await {
            log_store_error("deleting mailbox session", &mailbox, &e);
        }
        state.token_backend.forget(&mailbox);
        state.engines.forget(&self.signin);
        Ok(format!("Disconnected {mailbox}."))
    }

    async fn ai_setup(&self) -> OpResult<String> {
        let state = self.state()?;
        let key = state.ai_links.issue(&self.signin);
        tracing::info!(user = %user_hash(&self.signin), "ai link issued");
        Ok(format!(
            "Set up the user's own AI key by opening: {}/ai?state={key}  (valid {} minutes)\n\
             The user picks the provider and pastes their API key on that page; never ask for the key in the chat.",
            state.config.base_url(),
            crate::ai_page::LINK_TTL.num_minutes()
        ))
    }

    async fn deliver_attachment(
        &self,
        account: &str,
        message_id: &str,
        attachment: &Attachment,
        _bytes: Vec<u8>,
    ) -> OpResult<String> {
        let state = self.state()?;
        let generation = state
            .users
            .load(&self.signin)
            .await
            .ok()
            .flatten()
            .map(|r| r.token_generation)
            .unwrap_or_default();
        let token = state
            .signer
            .issue_download(&self.signin, generation, account, message_id, attachment)
            .map_err(|_| OpError::internal("could not create a download link"))?;
        Ok(format!(
            "Download {} ({} KB): {}/dl/{token}  (valid 15 minutes; give the user the link)",
            pidge_engine::render::inline(&attachment.name),
            attachment.size_bytes.div_ceil(1024),
            state.config.base_url()
        ))
    }

    async fn convert_document(
        &self,
        name: &str,
        _content_type: &str,
        bytes: Vec<u8>,
    ) -> Option<Result<String, String>> {
        let state = self.state().ok()?;
        let permit =
            tokio::time::timeout(Duration::from_secs(20), state.conversions.acquire()).await;
        let Ok(Ok(_permit)) = permit else {
            return Some(Err("the converter is busy; try again in a moment".into()));
        };
        Some(
            crate::markitdown::convert(
                &state.config.markitdown,
                &bytes,
                name,
                crate::markitdown::CONVERT_TIMEOUT,
            )
            .await
            .map_err(|e| e.to_string()),
        )
    }

    async fn one_click_unsubscribe(&self, url: &str) -> Result<(), String> {
        let state = self.state().map_err(|e| e.message)?;
        state
            .graph
            .unsubscribe_one_click(url)
            .await
            .map_err(|e| e.to_string())
    }

    async fn remove_ai_key(&self) -> OpResult<String> {
        let state = self.state()?;
        crate::ai::clear(&state.secrets, &self.signin)
            .await
            .map_err(|_| store_failure("remove the AI key"))?;
        state.engines.forget(&self.signin);
        Ok("The AI key is removed; pidge sorts with rules and mail signals only.".into())
    }

    async fn sign_out_everywhere(&self) -> OpResult<String> {
        let state = self.state()?;
        let mut record = state
            .users
            .load(&self.signin)
            .await
            .map_err(|_| store_failure("load your account"))?
            .ok_or_else(|| store_failure("load your account"))?;
        record.token_generation += 1;
        let generation = state
            .users
            .save(&record)
            .await
            .map_err(|_| store_failure("save your account"))?;
        state.set_generation(&self.signin, generation);
        tracing::info!(user = %user_hash(&self.signin), "signed out everywhere");
        Ok(
            "All sessions are signed out; every client, this one included, must sign in again."
                .into(),
        )
    }

    fn outbox_due(&self, _at: DateTime<Utc>) {}
}

/// The MCP side: who is calling, from the bearer token the HTTP layer
/// verified.
pub struct HttpSource {
    pub state: SharedState,
}

#[async_trait]
impl UserSource for HttpSource {
    async fn user(&self, ctx: &RequestContext<RoleServer>) -> Result<Arc<User>, String> {
        let signin = ctx
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<AuthenticatedUser>())
            .map(|u| u.email.clone())
            .ok_or_else(|| "request is not authenticated".to_string())?;
        self.state
            .engines
            .user(&self.state, &signin)
            .await
            .map_err(|e| {
                log_store_error("loading the user", &signin, &e);
                "could not load your account; try again shortly".to_string()
            })
    }

    fn log_call(&self, tool: &str, ctx: &RequestContext<RoleServer>, ms: u64, outcome: &str) {
        let user = ctx
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<AuthenticatedUser>())
            .map(|u| user_hash(&u.email))
            .unwrap_or_else(|| "-".to_string());
        tracing::info!(tool = %tool, user = %user, duration_ms = ms, outcome, "tool_call");
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{JANE, ToolHarness, WORK, access_token, fresh_tokens};
    use crate::users::{MailboxRecord, UserStore};

    #[tokio::test]
    async fn a_mailbox_whose_record_names_another_owner_never_reaches_the_engine() {
        let h = ToolHarness::new(&[JANE, WORK]).await;
        UserStore::new(h.secrets.clone())
            .save_mailbox(
                &MailboxRecord {
                    owner: "mallory@example.com".into(),
                    tokens: pidge_client::auth::TokenSet {
                        access_token: access_token(WORK),
                        ..fresh_tokens()
                    },
                    identity: None,
                    provider: Default::default(),
                },
                WORK,
            )
            .await
            .unwrap();
        let user = h.state.engines.user(&h.state, JANE).await.unwrap();
        assert_eq!(user.addresses(), vec![JANE.to_string()]);
        assert!(user.provider(WORK).is_err());
    }

    #[tokio::test]
    async fn the_engine_user_is_kept_and_refreshed_after_forget() {
        let h = ToolHarness::new(&[JANE]).await;
        let a = h.state.engines.user(&h.state, JANE).await.unwrap();
        h.state.engines.forget(JANE);
        let b = h.state.engines.user(&h.state, JANE).await.unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&a, &b),
            "the same user (worker, outbox) is kept"
        );
    }

    #[tokio::test]
    async fn legacy_review_categories_seed_the_preferences() {
        let h = ToolHarness::new(&[JANE]).await;
        let mut rec = h.record().await;
        rec.review = Some(serde_json::json!({
            "categories": [
                { "name": "Uppdrag", "description": "Client work", "rules": ["domain:kund.se"],
                  "examples": [{ "from": "Kund <a@kund.se>", "subject": "Offert" }] },
                { "name": "Promotions", "description": "Ads", "bulk_suggestion": true }
            ]
        }));
        h.state.users.save(&rec).await.unwrap();
        let user = h.state.engines.user(&h.state, JANE).await.unwrap();
        let cats = user.prefs().await.categories();
        assert_eq!(cats[0].name, "Uppdrag");
        assert_eq!(cats[0].rules, vec!["domain:kund.se".to_string()]);
        assert!(cats[1].sweep);
        assert_eq!(cats[1].kinds, vec![pidge_engine::kinds::Kind::Promotion]);
    }
}
