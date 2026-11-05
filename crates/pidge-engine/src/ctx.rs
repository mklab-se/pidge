//! The per-user runtime every operation runs against.
//!
//! A [`User`] holds what the frontend handed over (the user's mailboxes as
//! providers, their index, their preferences store, a [`Host`] for the few
//! things that differ between the CLI and the servers) plus the background
//! worker that makes mailbox changes fire-and-forget.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
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

    /// The outbox has an entry due at `at`. MCP frontends ignore this (the
    /// engine's own timer sends it); the CLI starts a detached runner.
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
            outbox_timer: std::sync::Mutex::new(None),
        })
    }

    pub fn frontend(&self) -> Frontend {
        self.host.frontend()
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
                .with_hint("accounts lists them")
            })
    }

    /// All mailboxes, or the one named (which must be the user's own).
    pub fn scope(&self, account: Option<&str>) -> OpResult<Vec<Arc<dyn Provider>>> {
        let all = self.providers();
        if all.is_empty() {
            return Err(OpError::new(ErrorKind::NotFound, "no mailbox is connected yet")
                .with_hint("accounts action=connect adds one"));
        }
        match account.map(str::trim).filter(|a| !a.is_empty() && *a != "all") {
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

    pub async fn prefs(&self) -> Prefs {
        let mut cached = self.prefs.lock().await;
        if let Some(p) = cached.as_ref() {
            return p.clone();
        }
        let loaded = match self.prefs_store.load().await {
            Ok(p) => p.unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = %e, "could not load preferences; using defaults");
                Prefs::default()
            }
        };
        *cached = Some(loaded.clone());
        loaded
    }

    /// Change preferences and save them.
    pub async fn update_prefs<T>(&self, f: impl FnOnce(&mut Prefs) -> T) -> OpResult<T> {
        let _ = self.prefs().await;
        let mut cached = self.prefs.lock().await;
        let prefs = cached.get_or_insert_with(Prefs::default);
        let out = f(prefs);
        self.prefs_store
            .save(prefs)
            .await
            .map_err(|e| OpError::internal(format!("could not save preferences: {e}")))?;
        Ok(out)
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
