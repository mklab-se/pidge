//! The per-user runtime every operation runs against.
//!
//! A [`User`] holds what the frontend handed over (the user's mailboxes as
//! providers, their index, their preferences store, a [`Host`] for the few
//! things that differ between the CLI and the servers) plus the background
//! worker that makes mailbox changes fire-and-forget.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use pidge_core::Attachment;
use pidge_core::provider::{Provider, ProviderKind};
use pidge_store::Store;
use tokio::sync::{Mutex, Notify, mpsc};

use crate::error::{ErrorKind, OpError, OpResult};
use crate::prefs::{Prefs, PrefsStore};

/// Which kind of frontend is running the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frontend {
    /// A short-lived CLI process: background work runs before it exits.
    Cli,
    /// A long-lived MCP server (stdio or HTTP): background work runs on the
    /// user's worker while the answer is already on its way.
    Mcp,
}

/// A mailbox as the accounts operation lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub address: String,
    pub provider: ProviderKind,
    /// `ok`, or why not (`needs reconnect`).
    pub status: String,
}

/// What differs between frontends. One per user.
#[async_trait]
pub trait Host: Send + Sync {
    fn frontend(&self) -> Frontend;

    /// The zone used when the user has not set one.
    fn default_timezone(&self) -> String;

    /// The user's mailboxes and their health.
    async fn accounts(&self) -> Vec<AccountInfo>;

    /// Start connecting a mailbox. Returns what to tell the user (a link
    /// to open, or the result of a local sign-in).
    async fn connect(&self, provider: ProviderKind, email: Option<&str>) -> OpResult<String>;

    /// Disconnect a mailbox (tokens removed).
    async fn disconnect(&self, address: &str) -> OpResult<String>;

    /// How the user sets up their own AI key (a link, or a command).
    async fn ai_setup(&self) -> OpResult<String>;

    /// Hand an attachment to the user: a download link (hosted) or a saved
    /// file (local). Returns the text to show.
    async fn deliver_attachment(
        &self,
        account: &str,
        message_id: &str,
        attachment: &Attachment,
        bytes: Vec<u8>,
    ) -> OpResult<String>;

    /// Convert a document to Markdown, when this frontend can.
    async fn convert_document(
        &self,
        _name: &str,
        _content_type: &str,
        _bytes: Vec<u8>,
    ) -> Option<Result<String, String>> {
        None
    }

    /// POST an RFC 8058 one-click unsubscribe (public https only).
    async fn one_click_unsubscribe(&self, url: &str) -> Result<(), String>;

    /// Revoke every session of the user (hosted only).
    async fn sign_out_everywhere(&self) -> OpResult<String> {
        Err(OpError::new(
            ErrorKind::Unsupported,
            "there are no server sessions to sign out of here",
        ))
    }

    /// Remove the user's own AI key.
    async fn remove_ai_key(&self) -> OpResult<String> {
        Ok("Run `pidge ai disable` in a terminal.".into())
    }

    /// Keep one activity line (see `activity`). Never fails: a line that
    /// cannot be kept is dropped with a warning.
    async fn record(&self, _line: String) {}

    /// Keep this user's index current between calls (a server that is always
    /// on), not only while they are active.
    fn always_on(&self) -> bool {
        false
    }

    /// The outbox has an entry due at `at` and nobody here to send it (a
    /// command that ends, a local server that stops): start a detached runner.
    fn outbox_due(&self, _at: DateTime<Utc>) {}
}

type Job = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Runs jobs in order, one at a time, so "archive, undo" stays in order.
struct Worker {
    tx: Option<mpsc::UnboundedSender<Job>>,
    pending: Arc<AtomicUsize>,
    idle: Arc<Notify>,
}

impl Worker {
    fn new(frontend: Frontend) -> Self {
        let pending = Arc::new(AtomicUsize::new(0));
        let idle = Arc::new(Notify::new());
        let tx = match frontend {
            Frontend::Cli => None,
            Frontend::Mcp => {
                let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
                let (pending, idle) = (pending.clone(), idle.clone());
                tokio::spawn(async move {
                    while let Some(job) = rx.recv().await {
                        job.await;
                        if pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                            idle.notify_waiters();
                        }
                    }
                });
                Some(tx)
            }
        };
        Self { tx, pending, idle }
    }
}

/// One user's engine state.
pub struct User {
    /// Stable id of the user (`local` for the CLI, a hash hosted).
    pub key: String,
    pub store: Arc<Store>,
    pub host: Arc<dyn Host>,
    prefs_store: Arc<dyn PrefsStore>,
    prefs: Mutex<Option<Prefs>>,
    providers: RwLock<Vec<Arc<dyn Provider>>>,
    ai: RwLock<Option<Arc<ailloy::Client>>>,
    worker: Worker,
    /// One sync per mailbox at a time.
    pub(crate) sync_locks: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// One run of the queue at a time.
    pub(crate) queue_lock: Mutex<()>,
    /// One round of background sorting at a time.
    pub(crate) sorting: Mutex<()>,
    /// When this process started serving the user.
    pub(crate) started: DateTime<Utc>,
    /// The instructions this process took (others may share the index).
    pub(crate) own_instructions: std::sync::Mutex<Vec<i64>>,
    /// When the user's agent last called (seconds since the epoch).
    last_call: AtomicI64,
    /// The task that keeps the index current.
    pub(crate) keeper: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// One outbox timer at a time.
    pub(crate) outbox_timer: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl User {
    pub fn new(
        key: impl Into<String>,
        store: Arc<Store>,
        prefs_store: Arc<dyn PrefsStore>,
        providers: Vec<Arc<dyn Provider>>,
        host: Arc<dyn Host>,
    ) -> Arc<Self> {
        let frontend = host.frontend();
        Arc::new(Self {
            key: key.into(),
            store,
            host,
            prefs_store,
            prefs: Mutex::new(None),
            providers: RwLock::new(providers),
            ai: RwLock::new(None),
            worker: Worker::new(frontend),
            sync_locks: std::sync::Mutex::new(HashMap::new()),
            queue_lock: Mutex::new(()),
            sorting: Mutex::new(()),
            started: Utc::now(),
            own_instructions: std::sync::Mutex::new(Vec::new()),
            last_call: AtomicI64::new(0),
            keeper: std::sync::Mutex::new(None),
            outbox_timer: std::sync::Mutex::new(None),
        })
    }

    pub fn frontend(&self) -> Frontend {
        self.host.frontend()
    }

    /// Hand an activity line to the host, after the answer: inline on the
    /// CLI, on the user's worker on a server.
    pub async fn record(self: &Arc<Self>, line: String) {
        let u = self.clone();
        self.enqueue(async move { u.host.record(line).await }).await;
    }

    /// Get a new database ready: its handles start where no earlier one's
    /// were, and what a stopped server left in the queue carries on. Call
    /// once, before the first operation.
    pub async fn prepare(self: &Arc<Self>) {
        const PER_DATABASE: u64 = 20_000;
        if self.store.is_fresh().unwrap_or(false) {
            let base = self
                .update_prefs(|p| {
                    p.handle_base += PER_DATABASE;
                    p.handle_base
                })
                .await
                // Preferences unreachable: the clock is the next best.
                .unwrap_or_else(|_| u64::try_from(Utc::now().timestamp()).unwrap_or_default());
            let _ = self.store.start_handles_at(base);
        }
        let _ = self.store.housekeeping();
        let _ = self
            .store
            .abandon_stuck(Utc::now() - chrono::Duration::minutes(10));
        crate::queue::resume(self).await;
    }

    pub(crate) fn called(&self) {
        self.last_call
            .store(Utc::now().timestamp(), Ordering::Relaxed);
    }

    /// Whether the user's agent has called in the last half hour.
    pub(crate) fn is_active(&self) -> bool {
        Utc::now().timestamp() - self.last_call.load(Ordering::Relaxed) < 30 * 60
    }

    /// Finish everything at once: the queue, mail approved and held, and the
    /// log (a server is stopping).
    pub async fn finish(self: &Arc<Self>) {
        crate::queue::flush(self).await;
        if self.host.always_on() {
            crate::outbox::flush(self).await;
        } else if let Ok(Some(at)) = self.store.next_due() {
            // The index outlives this process, and others may share it.
            self.host.outbox_due(at);
        }
        self.drain().await;
    }

    // ---- providers --------------------------------------------------------

    pub fn set_providers(&self, providers: Vec<Arc<dyn Provider>>) {
        if let Ok(mut p) = self.providers.write() {
            *p = providers;
        }
    }

    pub fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.providers.read().map(|p| p.clone()).unwrap_or_default()
    }

    pub fn addresses(&self) -> Vec<String> {
        self.providers()
            .iter()
            .map(|p| p.address().to_string())
            .collect()
    }

    /// The provider for one of the user's mailboxes.
    pub fn provider(&self, account: &str) -> OpResult<Arc<dyn Provider>> {
        self.providers()
            .into_iter()
            .find(|p| p.address().eq_ignore_ascii_case(account))
            .ok_or_else(|| {
                OpError::new(
                    ErrorKind::NotFound,
                    format!("{account} is not one of your mailboxes"),
                )
                .with_hint("settings lists them")
            })
    }

    /// All mailboxes, or the one named (which must be the user's own).
    pub fn scope(&self, account: Option<&str>) -> OpResult<Vec<Arc<dyn Provider>>> {
        let all = self.providers();
        if all.is_empty() {
            return Err(
                OpError::new(ErrorKind::NotFound, "no mailbox is connected yet")
                    .with_hint("settings action=connect adds one"),
            );
        }
        match account
            .map(str::trim)
            .filter(|a| !a.is_empty() && *a != "all")
        {
            None => Ok(all),
            Some(a) => Ok(vec![self.provider(a)?]),
        }
    }

    // ---- AI ---------------------------------------------------------------

    pub fn set_ai(&self, client: Option<Arc<ailloy::Client>>) {
        if let Ok(mut a) = self.ai.write() {
            *a = client;
        }
    }

    pub fn ai(&self) -> Option<Arc<ailloy::Client>> {
        self.ai.read().ok().and_then(|a| a.clone())
    }

    // ---- prefs ------------------------------------------------------------

    /// The preferences; the defaults while they cannot be read (never kept,
    /// so that a hiccup is not saved over what the user has).
    pub async fn prefs(&self) -> Prefs {
        let mut cached = self.prefs.lock().await;
        match self.loaded(&mut cached).await {
            Ok(p) => p.clone(),
            Err(e) => {
                tracing::warn!(error = %e, "could not load preferences; using defaults");
                Prefs::default()
            }
        }
    }

    async fn loaded<'a>(&self, cached: &'a mut Option<Prefs>) -> Result<&'a mut Prefs, String> {
        if cached.is_none() {
            *cached = Some(self.prefs_store.load().await?.unwrap_or_default());
        }
        Ok(cached.get_or_insert_with(Prefs::default))
    }

    /// Change preferences and save them.
    pub async fn update_prefs<T>(&self, f: impl FnOnce(&mut Prefs) -> T) -> OpResult<T> {
        let mut cached = self.prefs.lock().await;
        let prefs = self
            .loaded(&mut cached)
            .await
            .map_err(|e| OpError::internal(format!("could not load preferences: {e}")))?;
        let out = f(prefs);
        self.prefs_store
            .save(prefs)
            .await
            .map_err(|e| OpError::internal(format!("could not save preferences: {e}")))?;
        Ok(out)
    }

    /// Questions the user's AI may still be asked today, out of `n`; counted
    /// as asked. Kept in the preferences, so a restart does not start over.
    pub(crate) async fn ai_allowance(&self, n: usize) -> usize {
        const PER_DAY: u32 = 150;
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let wanted = u32::try_from(n).unwrap_or(u32::MAX);
        self.update_prefs(|p| {
            if p.ai.day != today {
                p.ai.day = today;
                p.ai.asked = 0;
            }
            let granted = wanted.min(PER_DAY.saturating_sub(p.ai.asked));
            p.ai.asked += granted;
            granted
        })
        .await
        .map_or(0, |n| n as usize)
    }

    /// Forget cached preferences (another process changed them).
    pub async fn reload_prefs(&self) {
        *self.prefs.lock().await = None;
    }

    pub async fn tz(&self) -> Tz {
        let prefs = self.prefs().await;
        prefs
            .timezone
            .as_deref()
            .and_then(|z| z.parse().ok())
            .or_else(|| self.host.default_timezone().parse().ok())
            .unwrap_or(chrono_tz::Europe::Stockholm)
    }

    // ---- background work ---------------------------------------------------

    /// Run `job` after everything queued before it. On the CLI it runs now;
    /// on a server the caller's answer does not wait for it.
    pub async fn enqueue(&self, job: impl Future<Output = ()> + Send + 'static) {
        match &self.worker.tx {
            None => job.await,
            Some(tx) => {
                self.worker.pending.fetch_add(1, Ordering::SeqCst);
                if tx.send(Box::pin(job)).is_err() {
                    self.worker.pending.fetch_sub(1, Ordering::SeqCst);
                }
            }
        }
    }

    /// Wait until the queue is empty (tests, shutdown).
    pub async fn drain(&self) {
        loop {
            let notified = self.worker.idle.notified();
            if self.worker.pending.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn sync_lock(&self, account: &str) -> Arc<Mutex<()>> {
        let mut map = self.sync_locks.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(account.to_ascii_lowercase())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}
