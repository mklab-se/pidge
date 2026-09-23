//! Converts attachments to Markdown with Microsoft's `markitdown`, run as a
//! subprocess (spec §1.10). The bytes go to a fresh per-conversion work
//! directory (also the child's `HOME`/`TMPDIR`) that is deleted on every
//! path, including a timeout or the call being dropped; nothing else is
//! stored. The child is killed if it outlives the timeout.

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

/// Address-space limit for the markitdown process (Linux only). It bounds
/// reserved address space, not resident memory, so it sits well above what
/// Python and its native libraries map at start-up; the output cap and the
/// timeout are the practical bounds.
#[cfg(target_os = "linux")]
const MAX_CHILD_MEMORY: libc::rlim_t = 4 * 1024 * 1024 * 1024;

/// How long one conversion may run.
pub const CONVERT_TIMEOUT: Duration = Duration::from_secs(30);

/// Each conversion's work directory is `<TEMP_PREFIX><random>` under the
/// system temp dir.
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
    let staged = WorkDir::create().await;
    let (work, input) = match staged {
        Ok(work) => match work.write_input(filename, bytes).await {
            Ok(input) => (work, input),
            Err(_) => return Err(staging_failed()),
        },
        Err(_) => return Err(staging_failed()),
    };

    let mut command = Command::new(bin);
    command
        .arg(&input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    scrub_environment(&mut command, &work.0);
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

fn staging_failed() -> ConvertError {
    ConvertError::Failed("could not stage the attachment".into())
}

/// The child gets none of the server's environment (on Container Apps that
/// holds the managed-identity endpoint and secret): only what Python needs.
/// `HOME` and `TMPDIR` are the conversion's own work directory, so caches
/// and scratch files the child writes go away with it.
fn scrub_environment(command: &mut Command, work: &Path) {
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command
        .env("HOME", work)
        .env("TMPDIR", work)
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        // markitdown's ML runtime otherwise starts a native thread per core
        // at import; under the address-space limit that aborts the process.
        .env("OMP_NUM_THREADS", "1")
        .env("OPENBLAS_NUM_THREADS", "1")
        .env("MKL_NUM_THREADS", "1")
        .env("TOKENIZERS_PARALLELISM", "false");
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

/// `pidge-mcp --convert-check <path>`: converts a local file exactly as
/// `mail_attachment` would (same spawn path, limits and timeout) and returns
/// the number of characters produced, or the error as text.
pub async fn convert_check(bin: &Path, path: &Path) -> Result<usize, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    convert(bin, &bytes, &name, CONVERT_TIMEOUT)
        .await
        .map(|markdown| markdown.chars().count())
        .map_err(|e| e.to_string())
}

/// Deletes our temp entries (work directories, and files from older
/// versions) older than `max_age`: the ones a killed process never got to
/// remove. Best effort; returns how many were deleted.
pub fn sweep_stale_temp_files(max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            continue;
        }
        // `symlink_metadata`: a link is removed as a link, never followed.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if !stale {
            continue;
        }
        let gone = if meta.is_dir() {
            std::fs::remove_dir_all(entry.path())
        } else {
            std::fs::remove_file(entry.path())
        };
        if gone.is_ok() {
            removed += 1;
        }
    }
    removed
}

/// A fresh `<prefix><random>` path under the system temp dir.
fn work_dir_path() -> PathBuf {
    let random: String = random_bytes(16)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    std::env::temp_dir().join(format!("{TEMP_PREFIX}{random}"))
}

/// `attachment.<ext>`, where `ext` is the original extension lower-cased
/// and cut to `[a-z0-9]`, kept only if that leaves 1–8 characters
/// (markitdown picks its converter by extension); plain `attachment` if not.
fn input_name(filename: &str) -> String {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| {
            ext.chars()
                .map(|c| c.to_ascii_lowercase())
                .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                .collect::<String>()
        })
        .filter(|ext| (1..=8).contains(&ext.len()));
    match ext {
        Some(ext) => format!("attachment.{ext}"),
        None => "attachment".to_string(),
    }
}

/// A conversion's private directory (owner-only on Unix), holding the
/// input and serving as the child's `HOME`/`TMPDIR`. Removed with
/// everything in it when dropped: on success, error, timeout, or the whole
/// call being cancelled.
struct WorkDir(PathBuf);

impl WorkDir {
    /// Creates a new directory; fails rather than reuse an existing one.
    async fn create() -> std::io::Result<Self> {
        let path = work_dir_path();
        let mut builder = tokio::fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&path).await?;
        Ok(Self(path))
    }

    /// Writes `bytes` to a new owner-only file named by [`input_name`].
    async fn write_input(&self, filename: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let path = self.0.join(input_name(filename));
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut f = options.open(&path).await?;
        f.write_all(bytes).await?;
        f.flush().await?;
        Ok(path)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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
        const ALLOWED: [&str; 14] = [
            "PATH",
            "HOME",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "PYTHONDONTWRITEBYTECODE",
            "OMP_NUM_THREADS",
            "OPENBLAS_NUM_THREADS",
            "MKL_NUM_THREADS",
            "TOKENIZERS_PARALLELISM",
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
        // HOME and TMPDIR are the conversion's own work directory.
        let value = |key: &str| {
            env.lines()
                .find_map(|l| l.strip_prefix(&format!("{key}=")))
                .unwrap_or_else(|| panic!("no {key}: {env}"))
                .to_string()
        };
        let work = value("TMPDIR");
        let prefix = std::env::temp_dir().join(TEMP_PREFIX);
        assert!(work.starts_with(&*prefix.to_string_lossy()), "{work}");
        assert_eq!(value("HOME"), work);
        assert!(env.contains("LANG=C.UTF-8\n"), "{env}");
        assert!(env.contains("LC_ALL=C.UTF-8\n"), "{env}");
        assert!(env.contains("PYTHONDONTWRITEBYTECODE=1\n"), "{env}");
        // One native thread per library: markitdown's ML runtime otherwise
        // starts one per core at import, which the address-space limit kills.
        for var in ["OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS"] {
            assert!(env.contains(&format!("{var}=1\n")), "{var}: {env}");
        }
        assert!(env.contains("TOKENIZERS_PARALLELISM=false\n"), "{env}");
    }

    #[tokio::test]
    async fn whatever_the_child_writes_under_home_is_removed_with_its_work_dir() {
        const CACHE: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/cache-markitdown.sh"
        );
        let _guard = conversion_lock().await;
        let before = temp_files();
        let home = convert(CACHE.as_ref(), b"x", "a.pdf", Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!home.is_empty());
        assert!(!Path::new(&home).exists(), "{home} survived");
        assert_eq!(temp_files(), before, "temp entry left behind");
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
    async fn the_child_runs_under_a_4_gib_address_space_limit() {
        const LIMITS: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/limits-markitdown.sh"
        );
        let _guard = conversion_lock().await;
        let out = convert(LIMITS.as_ref(), b"x", "a.pdf", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out.trim(), "4194304");
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
        let stale_dir = dir.join(format!("{TEMP_PREFIX}sweep-test-dir"));
        std::fs::create_dir_all(stale_dir.join(".cache/pip")).unwrap();
        std::fs::write(stale_dir.join(".cache/pip/entry"), b"x").unwrap();
        let two_hours_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        for p in [&stale, &other] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(two_hours_ago)
                .unwrap();
        }
        std::fs::File::open(&stale_dir)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();

        let removed = sweep_stale_temp_files(Duration::from_secs(60 * 60));
        assert!(removed >= 2, "{removed}");
        assert!(!stale.exists(), "stale temp file survived");
        assert!(!stale_dir.exists(), "stale work dir survived");
        assert!(fresh.exists(), "fresh temp file removed");
        assert!(other.exists(), "someone else's file removed");
        for p in [&fresh, &other] {
            std::fs::remove_file(p).unwrap();
        }
    }

    #[tokio::test]
    async fn convert_check_reports_characters_or_the_error() {
        let _guard = conversion_lock().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        // "# converted\nhello" is 17 characters.
        assert_eq!(convert_check(FAKE.as_ref(), &file).await, Ok(17));

        let broken = dir.path().join("b.fail");
        std::fs::write(&broken, "x").unwrap();
        let err = convert_check(FAKE.as_ref(), &broken).await.unwrap_err();
        assert!(err.contains("exit status: 3"), "{err}");

        let err = convert_check(FAKE.as_ref(), &dir.path().join("missing.txt"))
            .await
            .unwrap_err();
        assert!(err.starts_with("cannot read "), "{err}");
    }

    #[test]
    fn input_names_keep_a_sanitised_extension_only() {
        assert_eq!(input_name("Quarterly Report.PDF"), "attachment.pdf");
        assert_eq!(input_name("a.tar.gz"), "attachment.gz");
        assert_eq!(input_name("../../etc/passwd.dOcX"), "attachment.docx");
        assert_eq!(input_name("mixed.p-d_f"), "attachment.pdf");
        // Nothing usable: no extension at all rather than a guess.
        for f in ["noext", "weird.#$%", "long.abcdefghij", "trailing."] {
            assert_eq!(input_name(f), "attachment", "{f}");
        }
    }

    #[test]
    fn work_dirs_are_unique_and_prefixed_under_the_temp_dir() {
        let a = work_dir_path();
        assert!(a.starts_with(std::env::temp_dir()));
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(TEMP_PREFIX), "{name}");
        assert_ne!(a, work_dir_path(), "names must be unique");
    }
}
