//! `pidge account migrate-storage <email> --to <keychain|file>` — move credentials
//! between the OS keychain and the plaintext file backend without forcing a re-login.

use anyhow::{Result, anyhow};
use colored::Colorize;

use pidge_client::auth::TokenStore;
use pidge_core::{Config, TokenStorage};

pub fn run(email: String, to: TokenStorage) -> Result<()> {
    let mut config = Config::load()?;
    let current = config
        .find(&email)
        .map(|a| a.storage)
        .ok_or_else(|| anyhow!("not signed in to {email}"))?;

    if current == to {
        println!(
            "{} {email} already stores tokens in {}.",
            "✔".green(),
            backend_label(to)
        );
        return Ok(());
    }

    let tokens = TokenStore::load(&email, current)?.ok_or_else(|| {
        anyhow!(
            "no tokens found in the {} backend for {email} — try `pidge account add --store={}`",
            backend_label(current),
            backend_arg(to),
        )
    })?;

    TokenStore::save(&email, &tokens, to)?;

    let mut acct = config
        .find(&email)
        .cloned()
        .expect("account presence checked above");
    acct.storage = to;
    config.add_account(acct);
    config.save()?;

    if let Err(e) = TokenStore::delete(&email, current) {
        eprintln!(
            "{} migration succeeded but failed to clean up the old {} entry: {e}",
            "Warning:".yellow().bold(),
            backend_label(current),
        );
    }

    println!(
        "{} Migrated {email} from {} to {}.",
        "✔".green(),
        backend_label(current),
        backend_label(to),
    );
    if matches!(to, TokenStorage::File) {
        println!(
            "{} Refresh tokens are now in a plaintext file at \
             `~/.config/pidge/tokens/{email}.json` (or your OS equivalent). \
             Treat the file like a password.",
            "Note:".yellow().bold()
        );
    }
    Ok(())
}

fn backend_label(s: TokenStorage) -> &'static str {
    match s {
        TokenStorage::Keychain => "OS keychain",
        TokenStorage::File => "file",
    }
}

fn backend_arg(s: TokenStorage) -> &'static str {
    match s {
        TokenStorage::Keychain => "keychain",
        TokenStorage::File => "file",
    }
}
