//! `pidge auth login` — sign in to a Microsoft account via OAuth device code flow.

use anyhow::{Context, Result};
use chrono::Utc;
use colored::Colorize;

use pidge_client::auth::{extract_tenant_id, AuthClient, KeychainStore};
use pidge_core::{Account, Config};

pub async fn run() -> Result<()> {
    let auth = AuthClient::from_env().context("AuthClient initialisation failed")?;

    println!();
    println!("Adding a new account to pidge.");
    println!();

    let dc = auth
        .start_device_code()
        .await
        .context("failed to start device code flow")?;

    println!("  {}       {}", "Go to:".bold(), dc.verification_uri.cyan());
    println!("  {}  {}", "Enter code:".bold(), dc.user_code.bold().cyan());
    println!();
    println!(
        "{}",
        "Waiting for sign-in… (press Ctrl-C to cancel)".dimmed()
    );

    // Best-effort browser open
    let _ = open_browser(&dc.verification_uri);

    let success = auth
        .poll_for_tokens(&dc)
        .await
        .context("device code flow failed")?;

    // Tenant from id_token
    let tenant_id = success
        .id_token
        .as_deref()
        .and_then(extract_tenant_id)
        .unwrap_or_default();

    // Identity from Graph /me
    let graph = pidge_client::GraphClient::new(AuthClient::from_env()?)?;
    let me = graph
        .me(&success.tokens.access_token)
        .await
        .context("failed to fetch /me")?;
    let email = me
        .mail
        .clone()
        .unwrap_or_else(|| me.user_principal_name.clone());

    // Persist tokens
    KeychainStore::save(&email, &success.tokens)?;

    // Persist account in config
    let mut config = Config::load()?;
    let was_first = config.accounts.is_empty();
    config.add_account(Account {
        email: email.clone(),
        tenant_id,
        home_account_id: me.id,
        added_at: Utc::now(),
    });
    config.save()?;

    println!();
    println!(
        "{} {} <{}>",
        "✔".green(),
        "Signed in as".bold(),
        email
    );
    if was_first {
        println!();
        println!("This is your first account, so pidge has set it as:");
        println!("  • Default send-from account");
        println!("  • Default calendar account");
        println!();
        println!(
            "Change with {} or {}.",
            "`pidge auth default --send <email>`".cyan(),
            "`--calendar <email>`".cyan()
        );
    } else {
        println!("Currently signed in: {} accounts.", config.accounts.len());
    }

    Ok(())
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "windows")]
    let cmd = "start";

    std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(())
}
