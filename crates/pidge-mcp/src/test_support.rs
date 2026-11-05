//! Test-only helpers shared across modules: secret stores that fail always
//! or on demand, and an in-memory capture of this thread's tracing output.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tracing_subscriber::fmt::MakeWriter;

use crate::secrets::{SecretStore, SharedSecrets};

/// Fails every call with an error that names the secret and spells out the
/// address it was derived from, the worst case for log leakage.
pub struct FailingSecrets;

#[async_trait]
impl SecretStore for FailingSecrets {
    async fn get(&self, name: &str) -> Result<Option<String>> {
        Err(anyhow!(
            "reading secret {name} (jane@example.com): vault unavailable"
        ))
    }
    async fn set(&self, name: &str, _value: &str) -> Result<()> {
        Err(anyhow!(
            "writing secret {name} (jane@example.com): vault unavailable"
        ))
    }
}

/// Passes through to `inner` until told to fail: every read while
/// `fail_reads` is set, and writes to the one secret named in `fail_writes_to`.
pub struct FlakySecrets {
    inner: SharedSecrets,
    fail_reads: AtomicBool,
    fail_writes_to: Mutex<Option<String>>,
}

impl FlakySecrets {
    pub fn new(inner: SharedSecrets) -> Arc<Self> {
        Arc::new(Self {
            inner,
            fail_reads: AtomicBool::new(false),
            fail_writes_to: Mutex::new(None),
        })
    }

    pub fn fail_reads(&self, fail: bool) {
        self.fail_reads.store(fail, Ordering::SeqCst);
    }

    pub fn fail_writes_to(&self, name: Option<String>) {
        *self.fail_writes_to.lock().unwrap() = name;
    }
}

#[async_trait]
impl SecretStore for FlakySecrets {
    async fn get(&self, name: &str) -> Result<Option<String>> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return FailingSecrets.get(name).await;
        }
        self.inner.get(name).await
    }
    async fn set(&self, name: &str, value: &str) -> Result<()> {
        if self.fail_writes_to.lock().unwrap().as_deref() == Some(name) {
            return FailingSecrets.set(name, value).await;
        }
        self.inner.set(name, value).await
    }
}

/// Everything logged on this thread while the guard lives. Use from a
/// current-thread `#[tokio::test]` so every poll runs under the guard.
#[derive(Clone, Default)]
pub struct LogCapture(Arc<Mutex<Vec<u8>>>);

/// Serialises every test that installs a tracing subscriber, so two
/// captures never interleave and each full interest rebuild sees exactly
/// the subscriber whose test is running.
static LOG_LOCK: Mutex<()> = Mutex::new(());

/// Keeps one dispatcher registered with `tracing` for the whole test
/// process.
///
/// `tracing` caches each callsite's interest globally. When a callsite is
/// hit for the first time while at most one dispatcher is registered,
/// `tracing-core` computes its interest from the *hitting thread's* default
/// dispatcher. Test threads without a subscriber report "never", and that
/// verdict is cached, so a capturing test running at the same time silently
/// loses every event from that callsite until the next full rebuild. With a
/// second, permanently registered dispatcher the cache is instead computed
/// from the registered subscribers, all of which enable every level.
fn keep_registry_populated() {
    static KEEP_ALIVE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    KEEP_ALIVE.get_or_init(|| {
        let sink = tracing_subscriber::fmt()
            .with_writer(std::io::sink)
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        // `Dispatch::new` registers the dispatcher; forgetting it keeps the
        // registration alive for the rest of the process.
        std::mem::forget(tracing::Dispatch::new(sink));
    });
}

/// Keeps a captured subscriber installed (and the [`LOG_LOCK`] held) until
/// dropped. Field order matters: the subscriber guard drops first, then the
/// lock is released.
pub struct LogGuard {
    _subscriber: tracing::subscriber::DefaultGuard,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl LogCapture {
    pub fn start() -> (Self, LogGuard) {
        let capture = Self::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let guard = Self::install(subscriber);
        (capture, guard)
    }

    /// Installs `subscriber` as this thread's default under [`LOG_LOCK`],
    /// with the process-wide keep-alive dispatcher registered first.
    pub fn install<S>(subscriber: S) -> LogGuard
    where
        S: tracing::Subscriber + Send + Sync + 'static,
    {
        let lock = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        keep_registry_populated();
        // Registering a dispatcher rebuilds every cached interest from the
        // registered subscribers, which now always include the keep-alive.
        let subscriber = tracing::subscriber::set_default(subscriber);
        LogGuard {
            _subscriber: subscriber,
            _lock: lock,
        }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Asserts no part of `jane@example.com` appears in `text`.
pub fn assert_no_address(what: &str, text: &str) {
    for part in ["jane", "example"] {
        assert!(!text.contains(part), "{what} leaks `{part}`: {text}");
    }
}

/// Shared scaffolding for HTTP-level tests: an `AppState` over file secrets
/// and a wiremock that serves both Microsoft login (`/oauth2/v2.0/...`) and
/// Graph (`/v1.0/...`), with a saved user who owns `mailboxes`.
pub(crate) struct ToolHarness {
    pub graph: wiremock::MockServer,
    pub state: crate::state::SharedState,
    pub secrets: SharedSecrets,
    pub signin: String,
    _secrets_dir: tempfile::TempDir,
}

impl ToolHarness {
    pub async fn new(mailboxes: &[&str]) -> Self {
        use crate::users::{MailboxRecord, UserRecord, UserStore};
        let graph = wiremock::MockServer::start().await;
        let secrets_dir = tempfile::tempdir().unwrap();
        let secrets: SharedSecrets =
            Arc::new(crate::secrets::FileSecrets::new(secrets_dir.path()).unwrap());
        let signin = mailboxes[0].to_ascii_lowercase();
        let users = UserStore::new(secrets.clone());
        let mut record = UserRecord::new(&signin);
        record.mailboxes = mailboxes.iter().map(|m| m.to_ascii_lowercase()).collect();
        users.save(&record).await.unwrap();
        for m in &record.mailboxes {
            users
                .save_mailbox(
                    &MailboxRecord {
                        owner: signin.clone(),
                        tokens: pidge_client::auth::TokenSet {
                            access_token: access_token(m),
                            ..fresh_tokens()
                        },
                        identity: None,
                        provider: Default::default(),
                    },
                    m,
                )
                .await
                .unwrap();
        }
        let state = test_state(secrets.clone(), &graph.uri(), &signin, secrets_dir.path());
        Self {
            graph,
            state,
            secrets,
            signin,
            _secrets_dir: secrets_dir,
        }
    }
}

/// An `AppState` over `secrets`, with Microsoft login and Graph served by
/// `mock_uri` and `signin` the only allowlisted address.
pub(crate) fn test_state(
    secrets: SharedSecrets,
    mock_uri: &str,
    signin: &str,
    secrets_dir: &std::path::Path,
) -> crate::state::SharedState {
    use crate::config::{Config, EngineConfig, PrefsBackend, SecretsBackend};
    let config = Config {
        port: 8080,
        public_url: url::Url::parse("http://localhost:8080").unwrap(),
        allowed_emails: std::collections::HashSet::from([signin.to_string()]),
        secrets: SecretsBackend::File {
            dir: secrets_dir.to_path_buf(),
        },
        markitdown: crate::markitdown::tests::FAKE.into(),
        alt_hosts: Vec::new(),
        legacy_issuers: Vec::new(),
        log_format: crate::config::LogFormat::Text,
        redirect_hosts: Vec::new(),
        engine: EngineConfig {
            index_dir: secrets_dir.join("index"),
            prefs: PrefsBackend::Dir(secrets_dir.join("prefs")),
            google: None,
        },
    };
    let signer = crate::oauth::jwt::Signer::new(
        &crate::oauth::jwt::random_bytes(32),
        "http://localhost:8080",
        "http://localhost:8080/mcp",
    );
    let token_backend = Arc::new(crate::mailbox::SecretTokenBackend::new(secrets.clone()));
    let auth =
        pidge_client::AuthClient::for_test("cid", mock_uri).with_backend(token_backend.clone());
    let client = pidge_client::GraphClient::for_test(auth, format!("{mock_uri}/v1.0"));
    let mut state = crate::state::AppState::new(config, signer, client, token_backend, secrets);
    state.ai_endpoint = Some(mock_uri.to_string());
    Arc::new(state)
}

/// The access token [`ToolHarness::new`] stores for `mailbox`.
pub(crate) fn access_token(mailbox: &str) -> String {
    format!("AT-{mailbox}")
}

pub(crate) fn fresh_tokens() -> pidge_client::auth::TokenSet {
    pidge_client::auth::TokenSet {
        access_token: "AT".into(),
        refresh_token: "RT".into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        scope: None,
    }
}

pub(crate) const JANE: &str = "jane@example.com";
pub(crate) const WORK: &str = "work@example.com";

/// Graph serves `bytes` as attachment `attachment_id` of message `id` in
/// `mailbox`, `times` times.
pub(crate) async fn mount_bytes(
    h: &ToolHarness,
    mailbox: &str,
    id: &str,
    attachment_id: &str,
    bytes: &[u8],
    times: u64,
) {
    use base64::Engine;
    use wiremock::matchers::{header, method, path};
    wiremock::Mock::given(method("GET"))
        .and(path(format!(
            "/v1.0/me/messages/{id}/attachments/{attachment_id}"
        )))
        .and(header(
            "authorization",
            format!("Bearer {}", access_token(mailbox)).as_str(),
        ))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": attachment_id,
                "contentBytes": base64::engine::general_purpose::STANDARD.encode(bytes),
            })),
        )
        .expect(times)
        .mount(&h.graph)
        .await;
}

impl ToolHarness {
    /// The MCP server as `/mcp` runs it.
    pub fn mcp(&self) -> pidge_engine::mcp::PidgeServer<crate::engine::HttpSource> {
        pidge_engine::mcp::PidgeServer::new(crate::engine::HttpSource {
            state: self.state.clone(),
        })
    }

    /// A request context authenticated as the sign-in user.
    pub fn ctx(&self) -> rmcp::service::RequestContext<rmcp::RoleServer> {
        request_context(&self.signin)
    }
}

/// Answers nothing; exists only so rmcp hands us a real `Peer`.
struct NoopServer;
impl rmcp::ServerHandler for NoopServer {}

/// A `RequestContext` whose extensions carry `http::request::Parts` with an
/// `AuthenticatedUser`, as the bearer middleware and rmcp's HTTP transport
/// produce for a real call. Must run inside a Tokio runtime.
pub(crate) fn request_context(email: &str) -> rmcp::service::RequestContext<rmcp::RoleServer> {
    use rmcp::model::{Extensions, NumberOrString};
    let (transport, _unused) = tokio::io::duplex(64);
    let running = rmcp::service::serve_directly(NoopServer, transport, None);
    let mut ctx =
        rmcp::service::RequestContext::new(NumberOrString::Number(1), running.peer().clone());
    let (mut parts, ()) = http::Request::builder().body(()).unwrap().into_parts();
    parts
        .extensions
        .insert(crate::oauth::bearer::AuthenticatedUser {
            email: email.to_string(),
        });
    let mut extensions = Extensions::new();
    extensions.insert(parts);
    ctx.extensions = extensions;
    ctx
}

impl ToolHarness {
    pub async fn record(&self) -> crate::users::UserRecord {
        self.state.users.load(&self.signin).await.unwrap().unwrap()
    }

    /// A fresh state over `secrets` (e.g. a wrapper around [`Self::secrets`]),
    /// with the same Graph mock and sign-in user.
    pub fn over(&self, secrets: SharedSecrets) -> (crate::state::SharedState, ()) {
        (
            test_state(
                secrets,
                &self.graph.uri(),
                &self.signin,
                self._secrets_dir.path(),
            ),
            (),
        )
    }
}
