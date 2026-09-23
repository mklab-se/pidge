//! Test-only seam for pointing config-dir-based stores (`auth::file_store`,
//! `mcp::store`) at a temp directory without mutating process-wide env vars.
//!
//! `dirs::config_dir()` reads `HOME`/`XDG_CONFIG_HOME`, which are global to
//! the whole process. Two independent test modules each overriding those
//! vars behind their own lock can still race across threads within one test
//! binary — one test's `HOME` swap or restore can land mid-flight in
//! another. A thread-local override avoids the problem outright: it is
//! visible only to the thread that set it, so parallel `#[test]` functions
//! on different threads never interfere, and no lock is required at all.
//! See `crate::base_config_dir`, which consults this before falling back to
//! `dirs::config_dir()`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

thread_local! {
    static BASE_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Run `f` with the config-dir base pinned to `dir` for the current thread,
/// restoring whatever was there before (including across a panic inside
/// `f`) once `f` returns.
pub(crate) fn with_base_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let previous = BASE_DIR.with(|cell| cell.borrow_mut().replace(dir.to_path_buf()));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    BASE_DIR.with(|cell| *cell.borrow_mut() = previous);
    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// The override set by [`with_base_dir`] for the current thread, if any.
pub(crate) fn base_dir_override() -> Option<PathBuf> {
    BASE_DIR.with(|cell| cell.borrow().clone())
}
