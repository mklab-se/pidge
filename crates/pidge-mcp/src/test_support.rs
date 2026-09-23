//! Test-only helpers shared across modules: secret stores that fail always
//! or on demand, and an in-memory capture of this thread's tracing output.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tracing_subscriber::fmt::MakeWriter;

use crate::secrets::{SecretStore, SharedSecrets};

/// Fails every call with an error that names the secret and spells out the
/// address it was derived from — the worst case for log leakage.
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

impl LogCapture {
    pub fn start() -> (Self, tracing::subscriber::DefaultGuard) {
        let capture = Self::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        (capture, tracing::subscriber::set_default(subscriber))
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
