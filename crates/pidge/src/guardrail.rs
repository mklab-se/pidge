//! `--dry-run` for the few local commands that support it (`pidge mcp
//! connect`). Mail and calendar changes preview through plans instead.

static DRY_RUN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_dry_run() {
    DRY_RUN.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn dry_run_active() -> bool {
    DRY_RUN.load(std::sync::atomic::Ordering::SeqCst)
}
