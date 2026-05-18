//! `pidge account add` — sign in to a new Microsoft account via OAuth
//! authorization-code + PKCE with a one-shot localhost HTTP server.

use anyhow::{Context, Result};
use chrono::Utc;
use colored::Colorize;

use pidge_client::auth::{AuthClient, TokenStore, extract_tenant_id};
use pidge_core::{Account, Config, TokenStorage};

pub async fn run(storage: TokenStorage) -> Result<()> {
    let auth = AuthClient::from_env().context("AuthClient initialisation failed")?;

    println!();
    println!("Adding a new account to pidge.");
    if matches!(storage, TokenStorage::File) {
        println!(
            "{} Tokens will be saved as a {} on disk (mode 0600).",
            "Note:".yellow().bold(),
            "plaintext file".bold()
        );
    }
    println!();
    println!(
        "{}",
        "A browser window will open for sign-in. Both personal Microsoft \
         accounts (outlook.com / live.com / hotmail.com) and work/school \
         M365 accounts are supported."
            .dimmed()
    );
    println!();

    let success = auth
        .run_browser_flow(|authorize_url| {
            println!("{} {}", "Sign in at:".bold(), authorize_url.cyan());
            println!(
                "{}",
                "(opening your browser…  Ctrl-C here to cancel)".dimmed()
            );
            let _ = open_browser(authorize_url);
        })
        .await
        .context("browser sign-in failed")?;

    // Tenant from id_token (when present — Microsoft only returns id_token
    // if the `openid` scope was requested or as part of certain flows).
    let tenant_id = success
        .id_token
        .as_deref()
        .and_then(extract_tenant_id)
        .unwrap_or_default();

    // Identity from Graph /me — same as before; this is what teaches us the
    // user's actual e-mail address so we can key the cached tokens by it.
    let graph = pidge_client::GraphClient::new(auth)?;
    let me = graph
        .me(&success.tokens.access_token)
        .await
        .context("failed to fetch /me")?;
    let email = me
        .mail
        .clone()
        .unwrap_or_else(|| me.user_principal_name.clone());

    // Persist tokens to the requested backend
    TokenStore::save(&email, &success.tokens, storage)?;

    // Persist account in config
    let mut config = Config::load()?;
    let was_first = config.accounts.is_empty();
    config.add_account(Account {
        email: email.clone(),
        tenant_id,
        home_account_id: me.id,
        added_at: Utc::now(),
        storage,
    });
    config.save()?;

    println!();
    println!("{} {} <{}>", "✔".green(), "Signed in as".bold(), email);
    if was_first {
        println!();
        println!("This is your first account, so pidge has set it as:");
        println!("  • Default e-mail account");
        println!("  • Default calendar account");
        println!();
        println!(
            "Change with {} or {}.",
            "`pidge account default e-mail <email>`".cyan(),
            "`pidge account default calendar <email>`".cyan()
        );
    } else {
        println!("Currently signed in: {} accounts.", config.accounts.len());
    }

    Ok(())
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
    }
    #[cfg(target_os = "windows")]
    {
        // `start` is a cmd.exe built-in, not a standalone executable.
        std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
    }
    Ok(())
}
