//! Converts attachments to Markdown with Microsoft's `markitdown`, run as a
//! subprocess (spec §1.10). The bytes go to a fresh temp file that is
//! deleted on every path, including a timeout or the call being dropped;
//! nothing else is stored. The child is killed if it outlives the timeout.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::oauth::jwt::random_bytes;

/// The largest attachment pidge reads or links (spec §1.10).
pub const MAX_INPUT_BYTES: u64 = 25 * 1024 * 1024;

/// The most converted text pidge accepts from one conversion.
const MAX_OUTPUT_BYTES: u64 = 2 * 1024 * 1024;

/// Address-space limit for the markitdown process (Linux only).
#[cfg(target_os = "linux")]
const MAX_CHILD_MEMORY: libc::rlim_t = 1024 * 1024 * 1024;

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
/// to Markdown by running `bin` (from [`crate::config::Config::markitdown`])
/// for at most `timeout`.
pub async fn convert(
    bin: &Path,
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

    let mut command = Command::new(bin);
    command
        .arg(&file.0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    scrub_environment(&mut command);
    limit_resources(&mut command);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(ConvertError::Missing),
        Err(_) => {
            return Err(ConvertError::Failed(
                "markitdown could not be started".into(),
            ));
        }
    };
    let mut stdout = child.stdout.take().expect("stdout is piped");

    let run = async {
        let mut out = Vec::new();
        (&mut stdout)
            .take(MAX_OUTPUT_BYTES + 1)
            .read_to_end(&mut out)
            .await
            .map_err(|_| ConvertError::Failed("markitdown output unreadable".into()))?;
        if out.len() as u64 > MAX_OUTPUT_BYTES {
            let _ = child.kill().await;
            return Err(ConvertError::Failed("output too large".into()));
        }
        let status = child
            .wait()
            .await
            .map_err(|_| ConvertError::Failed("markitdown did not finish".into()))?;
        if !status.success() {
            return Err(ConvertError::Failed(format!(
                "markitdown failed ({status})"
            )));
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    };
    // On timeout `run` is dropped and `child` with it at return, which kills it.
    tokio::time::timeout(timeout, run)
        .await
        .unwrap_or(Err(ConvertError::Timeout))
}

/// The child gets none of the server's environment (on Container Apps that
/// holds the managed-identity endpoint and secret): only what Python needs.
fn scrub_environment(command: &mut Command) {
    let tmp = std::env::temp_dir();
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command
        .env("HOME", &tmp)
        .env("TMPDIR", &tmp)
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("PYTHONDONTWRITEBYTECODE", "1");
}

/// Caps the child's address space at [`MAX_CHILD_MEMORY`] on Linux (where
/// the server runs). Not applied elsewhere: macOS reserves more address
/// space than that for any process, so the limit would stop `exec` itself.
#[cfg(target_os = "linux")]
fn limit_resources(command: &mut Command) {
    // SAFETY: the closure runs in the forked child before exec and only
    // calls `setrlimit`, which is async-signal-safe; it allocates nothing.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: MAX_CHILD_MEMORY,
                rlim_max: MAX_CHILD_MEMORY,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn limit_resources(_command: &mut Command) {}

/// Deletes our temp files older than `max_age`: the ones a killed process
/// never got to remove. Best effort; returns how many were deleted.
pub fn sweep_stale_temp_files(max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > max_age)
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
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

    pub(crate) const FAKE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fake-markitdown.sh"
    );
    const SLOW: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/slow-markitdown.sh"
    );
    const ENV: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/env-markitdown.sh"
    );
    const BIG: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/big-markitdown.sh"
    );

    /// Serialises conversions across tests so temp-file counts are exact.
    /// Every test that converts holds the returned guard.
    pub(crate) async fn conversion_lock() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
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
        let _guard = conversion_lock().await;
        let before = temp_files();
        let out = convert(
            FAKE.as_ref(),
            b"hello, world\n",
            "Report.PDF",
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert_eq!(out, "# converted\nhello, world\n");
        assert_eq!(temp_files(), before, "temp file left behind");
    }

    #[tokio::test]
    async fn convert_times_out_kills_the_child_and_removes_the_temp_file() {
        let _guard = conversion_lock().await;
        let before = temp_files();
        let started = std::time::Instant::now();
        let err = convert(SLOW.as_ref(), b"x", "a.pdf", Duration::from_millis(300))
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
        let _guard = conversion_lock().await;
        let before = temp_files();
        let err = convert(
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
        let _guard = conversion_lock().await;
        let before = temp_files();
        let err = convert(FAKE.as_ref(), b"x", "broken.fail", Duration::from_secs(5))
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
        let _guard = conversion_lock().await;
        let big = vec![0u8; MAX_INPUT_BYTES as usize + 1];
        let err = convert(FAKE.as_ref(), &big, "big.pdf", Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(err, ConvertError::TooLarge);
    }

    #[tokio::test]
    async fn the_child_sees_a_scrubbed_environment() {
        let _guard = conversion_lock().await;
        let env = convert(ENV.as_ref(), b"x", "a.pdf", Duration::from_secs(5))
            .await
            .unwrap();
        // Only the variables we set, plus what the shell adds itself.
        const ALLOWED: [&str; 10] = [
            "PATH",
            "HOME",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "PYTHONDONTWRITEBYTECODE",
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
        ];
        for line in env.lines() {
            let key = line.split('=').next().unwrap();
            assert!(ALLOWED.contains(&key), "leaked {key}: {env}");
        }
        // Not vacuous: the test process has variables beyond that list.
        assert!(std::env::vars().any(|(k, _)| !ALLOWED.contains(&k.as_str())));
        let path = std::env::var("PATH").unwrap();
        assert!(env.contains(&format!("PATH={path}\n")), "{env}");
        let tmp = std::env::temp_dir();
        assert!(
            env.contains(&format!("TMPDIR={}\n", tmp.display())),
            "{env}"
        );
        assert!(env.contains(&format!("HOME={}\n", tmp.display())), "{env}");
        assert!(env.contains("LANG=C.UTF-8\n"), "{env}");
        assert!(env.contains("LC_ALL=C.UTF-8\n"), "{env}");
        assert!(env.contains("PYTHONDONTWRITEBYTECODE=1\n"), "{env}");
    }

    #[tokio::test]
    async fn output_over_2_mb_fails_with_a_fixed_message() {
        let _guard = conversion_lock().await;
        let before = temp_files();
        let err = convert(BIG.as_ref(), b"x", "a.pdf", Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(err, ConvertError::Failed("output too large".into()));
        assert_eq!(temp_files(), before, "temp file left behind");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_child_runs_under_a_1_gib_address_space_limit() {
        const LIMITS: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/limits-markitdown.sh"
        );
        let _guard = conversion_lock().await;
        let out = convert(LIMITS.as_ref(), b"x", "a.pdf", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out.trim(), "1048576");
    }

    #[tokio::test]
    async fn the_startup_sweep_removes_only_our_stale_temp_files() {
        let _guard = conversion_lock().await;
        let dir = std::env::temp_dir();
        let stale = dir.join(format!("{TEMP_PREFIX}sweep-test-stale.pdf"));
        let fresh = dir.join(format!("{TEMP_PREFIX}sweep-test-fresh.pdf"));
        let other = dir.join("not-pidge-sweep-test.pdf");
        for p in [&stale, &fresh, &other] {
            std::fs::write(p, b"x").unwrap();
        }
        let two_hours_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        for p in [&stale, &other] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(two_hours_ago)
                .unwrap();
        }

        let removed = sweep_stale_temp_files(Duration::from_secs(60 * 60));
        assert!(removed >= 1, "{removed}");
        assert!(!stale.exists(), "stale temp file survived");
        assert!(fresh.exists(), "fresh temp file removed");
        assert!(other.exists(), "someone else's file removed");
        for p in [&fresh, &other] {
            std::fs::remove_file(p).unwrap();
        }
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
