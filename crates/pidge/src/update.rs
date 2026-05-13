//! Version update checker
//!
//! Queries crates.io for the latest version of pidge, caches results for 24 hours,
//! and prints a notification if a newer version is available.

use std::io::Write;
use std::path::PathBuf;

use colored::Colorize;
use serde::{Deserialize, Serialize};
use tracing::debug;

const CRATE_NAME: &str = "pidge";
const CACHE_DURATION_HOURS: i64 = 24;

#[derive(Debug, Serialize, Deserialize)]
struct UpdateCache {
    latest_version: String,
    checked_at: String,
}

#[derive(Debug, Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    krate: CrateInfo,
}

#[derive(Debug, Deserialize)]
struct CrateInfo {
    max_stable_version: String,
}

fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("pidge").join("update-check.json"))
}

fn read_cache() -> Option<UpdateCache> {
    let path = cache_path()?;
    let data = std::fs::read_to_string(&path).ok()?;
    let cache: UpdateCache = serde_json::from_str(&data).ok()?;

    let checked_at = chrono::DateTime::parse_from_rfc3339(&cache.checked_at).ok()?;
    let age = chrono::Utc::now() - checked_at.to_utc();
    if age.num_hours() >= CACHE_DURATION_HOURS {
        debug!("update cache expired");
        return None;
    }

    Some(cache)
}

fn write_cache(latest_version: &str) {
    let Some(path) = cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cache = UpdateCache {
        latest_version: latest_version.to_string(),
        checked_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&path, json);
    }
}

async fn fetch_latest_version() -> Option<String> {
    let url = format!("https://crates.io/api/v1/crates/{CRATE_NAME}");
    let client = reqwest::Client::builder()
        .user_agent(format!("pidge/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;

    let resp: CratesIoResponse = client.get(&url).send().await.ok()?.json().await.ok()?;
    Some(resp.krate.max_stable_version)
}

fn detect_install_method() -> &'static str {
    if let Ok(exe) = std::env::current_exe() {
        let exe_str = exe.to_string_lossy();
        if exe_str.contains("homebrew")
            || exe_str.contains("Cellar")
            || exe_str.contains("linuxbrew")
        {
            return "brew upgrade pidge";
        }
    }

    if which_exists("cargo-binstall") {
        return "cargo binstall pidge";
    }

    "cargo install pidge"
}

fn which_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn print_update_notification(current: &semver::Version, latest: &semver::Version) {
    let update_cmd = detect_install_method();
    let _ = writeln!(
        std::io::stderr(),
        "\n{} {} → {} (update with: {})",
        "A new version of pidge is available:".yellow().bold(),
        current.to_string().dimmed(),
        latest.to_string().green().bold(),
        update_cmd.cyan(),
    );
}

/// Check for updates in the background. Returns a future that resolves
/// after checking and optionally printing a notification.
pub async fn check_for_updates() {
    let current_str = env!("CARGO_PKG_VERSION");
    let Ok(current) = semver::Version::parse(current_str) else {
        return;
    };

    let latest_str = if let Some(cache) = read_cache() {
        debug!(version = %cache.latest_version, "using cached version info");
        cache.latest_version
    } else {
        debug!("fetching latest version from crates.io");
        let Some(version) = fetch_latest_version().await else {
            debug!("failed to fetch latest version");
            return;
        };
        write_cache(&version);
        version
    };

    let Ok(latest) = semver::Version::parse(&latest_str) else {
        return;
    };

    if latest > current {
        print_update_notification(&current, &latest);
    } else {
        debug!(current = %current, latest = %latest, "pidge is up to date");
    }
}
