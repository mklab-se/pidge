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
