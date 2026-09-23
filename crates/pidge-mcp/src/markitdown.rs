//! Converts attachments to Markdown with Microsoft's `markitdown`, run as a
//! subprocess (spec §1.10). The bytes go to a fresh temp file that is
//! deleted on every path, including a timeout or the call being dropped;
//! nothing else is stored. The child is killed if it outlives the timeout.

use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::oauth::jwt::random_bytes;

/// The largest attachment pidge reads or links (spec §1.10).
pub const MAX_INPUT_BYTES: u64 = 25 * 1024 * 1024;

/// Temp files are named `<TEMP_PREFIX><random>.<ext>`.
const TEMP_PREFIX: &str = "pidge-mcp-att-";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConvertError {
    #[error("conversion timed out")]
    Timeout,
    #[error("attachment is over the size limit")]
    TooLarge,
    #[error("markitdown is not installed")]
    Missing,
    /// A short fixed description plus the exit status; never the child's
    /// stderr, which can quote the document.
    #[error("{0}")]
    Failed(String),
}

/// Converts `bytes` (named `filename`, whose extension guides markitdown)
/// to Markdown, running the binary named by `PIDGE_MCP_MARKITDOWN`
/// (default `markitdown`) for at most `timeout`.
pub async fn convert(
    bytes: &[u8],
    filename: &str,
    timeout: Duration,
) -> Result<String, ConvertError> {
    convert_with(&binary(), bytes, filename, timeout).await
}

async fn convert_with(
    bin: &OsStr,
    bytes: &[u8],
    filename: &str,
    timeout: Duration,
) -> Result<String, ConvertError> {
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ConvertError::TooLarge);
    }
    let file = TempFile::write(temp_path(filename), bytes)
        .await
        .map_err(|_| ConvertError::Failed("could not stage the attachment".into()))?;

    let child = Command::new(bin)
        .arg(&file.0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(ConvertError::Missing),
        Err(_) => {
            return Err(ConvertError::Failed(
                "markitdown could not be started".into(),
            ));
        }
    };
    // On timeout the future (and with it the child) is dropped, which kills it.
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => return Err(ConvertError::Timeout),
        Ok(Err(_)) => return Err(ConvertError::Failed("markitdown did not finish".into())),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        return Err(ConvertError::Failed(format!(
            "markitdown failed ({})",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn binary() -> OsString {
    std::env::var_os("PIDGE_MCP_MARKITDOWN").unwrap_or_else(|| "markitdown".into())
}

/// `<prefix><random>.<ext>` under the system temp dir, where `ext` is the
/// original extension lower-cased and cut to `[a-z0-9]`, kept only if that
/// leaves 1–8 characters (markitdown picks its converter by extension).
fn temp_path(filename: &str) -> PathBuf {
    let random: String = random_bytes(16)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| {
            ext.chars()
                .map(|c| c.to_ascii_lowercase())
                .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                .collect::<String>()
        })
        .filter(|ext| (1..=8).contains(&ext.len()));
    let name = match ext {
        Some(ext) => format!("{TEMP_PREFIX}{random}.{ext}"),
        None => format!("{TEMP_PREFIX}{random}"),
    };
    std::env::temp_dir().join(name)
}

/// A file we created, removed when dropped: on success, error, timeout, or
/// the whole call being cancelled.
struct TempFile(PathBuf);

impl TempFile {
    /// Creates `path` exclusively (owner-only on Unix) and writes `bytes`.
    async fn write(path: PathBuf, bytes: &[u8]) -> std::io::Result<Self> {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut f = options.open(&path).await?;
        // Owned from here on, so a failed write still cleans up.
        let file = Self(path);
        f.write_all(bytes).await?;
        f.flush().await?;
        Ok(file)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const FAKE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fake-markitdown.sh"
    );
    const SLOW: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/slow-markitdown.sh"
    );

    /// Points `PIDGE_MCP_MARKITDOWN` at the fake script for the whole test
    /// process, and serialises conversions so temp-file counts are exact.
    /// Every test that converts holds the returned guard.
    pub(crate) async fn fake_markitdown() -> tokio::sync::MutexGuard<'static, ()> {
        static SET: std::sync::Once = std::sync::Once::new();
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        SET.call_once(|| {
            // SAFETY: set once, always to the same value, before any
            // conversion reads it; every reader goes through `std::env`,
            // which serialises access to the environment.
            unsafe { std::env::set_var("PIDGE_MCP_MARKITDOWN", FAKE) };
        });
        LOCK.lock().await
    }

    /// How many of our temp files exist right now.
    pub(crate) fn temp_files() -> usize {
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .count()
    }

    #[tokio::test]
    async fn convert_returns_markitdown_stdout_and_removes_the_temp_file() {
        let _guard = fake_markitdown().await;
        let before = temp_files();
        let out = convert(b"hello, world\n", "Report.PDF", Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(out, "# converted\nhello, world\n");
        assert_eq!(temp_files(), before, "temp file left behind");
    }

    #[tokio::test]
    async fn convert_times_out_kills_the_child_and_removes_the_temp_file() {
        let _guard = fake_markitdown().await;
        let before = temp_files();
        let started = std::time::Instant::now();
        let err = convert_with(SLOW.as_ref(), b"x", "a.pdf", Duration::from_millis(300))
            .await
            .unwrap_err();
        assert_eq!(err, ConvertError::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "did not stop at the timeout"
        );
        assert_eq!(temp_files(), before, "temp file left behind");
    }

    #[tokio::test]
    async fn a_missing_binary_is_missing_and_removes_the_temp_file() {
        let _guard = fake_markitdown().await;
        let before = temp_files();
        let err = convert_with(
            "/nonexistent/pidge/markitdown".as_ref(),
            b"x",
            "a.pdf",
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert_eq!(err, ConvertError::Missing);
        assert_eq!(temp_files(), before, "temp file left behind");
    }

    #[tokio::test]
    async fn a_failed_conversion_reports_the_status_but_never_stderr() {
        let _guard = fake_markitdown().await;
        let before = temp_files();
        let err = convert(b"x", "broken.fail", Duration::from_secs(5))
            .await
            .unwrap_err();
        let ConvertError::Failed(msg) = &err else {
            panic!("expected Failed, got {err:?}");
        };
        assert!(msg.contains("exit status: 3"), "{msg}");
        assert!(!msg.contains("secret-stderr-detail"), "{msg}");
        assert_eq!(temp_files(), before, "temp file left behind");
    }

    #[tokio::test]
    async fn input_over_the_limit_is_refused_without_running_anything() {
        let _guard = fake_markitdown().await;
        let big = vec![0u8; MAX_INPUT_BYTES as usize + 1];
        let err = convert(&big, "big.pdf", Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(err, ConvertError::TooLarge);
    }

    #[test]
    fn temp_names_keep_a_sanitised_extension_only() {
        let name = |f: &str| {
            temp_path(f)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        assert!(temp_path("x.pdf").starts_with(std::env::temp_dir()));
        let pdf = name("Quarterly Report.PDF");
        assert!(
            pdf.starts_with(TEMP_PREFIX) && pdf.ends_with(".pdf"),
            "{pdf}"
        );
        assert!(name("a.tar.gz").ends_with(".gz"));
        assert!(name("../../etc/passwd.dOcX").ends_with(".docx"));
        // Nothing usable: no extension at all rather than a guess.
        for f in ["noext", "weird.#$%", "long.abcdefghij", "trailing."] {
            let n = name(f);
            assert!(!n.contains('.'), "{f} -> {n}");
        }
        assert!(name("mixed.p-d_f").ends_with(".pdf"));
        assert_ne!(name("a.pdf"), name("a.pdf"), "names must be unique");
    }
}
