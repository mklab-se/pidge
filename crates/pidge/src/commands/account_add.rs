//! `pidge account add`: sign in to a Microsoft or Google mailbox through
//! the browser (authorization code + PKCE, one-shot localhost listener).

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use colored::Colorize;

use pidge_client::auth::{AuthClient, TokenStore, extract_tenant_id};
use pidge_core::provider::ProviderKind;
use pidge_core::{Account, Config, TokenStorage};

/// The Google OAuth client: the environment wins over `config.yaml`.
pub fn google_client(config: &Config) -> Option<pidge_gmail::GoogleOAuthConfig> {
    pidge_gmail::GoogleOAuthConfig::from_env().or_else(|| {
        config
            .google
            .as_ref()
            .filter(|g| !g.client_id.is_empty())
            .map(|g| {
                pidge_gmail::GoogleOAuthConfig::new(g.client_id.clone(), g.client_secret.clone())
            })
    })
}

/// Sign in through the browser and record the mailbox. `on_url` gets the
/// sign-in link (print it, open it). Returns the address.
pub async fn sign_in(
    provider: ProviderKind,
    storage: TokenStorage,
    on_url: impl FnOnce(&str),
) -> Result<String> {
    let mut config = Config::load()?;
    let (email, tenant_id, home_account_id, tokens) = match provider {
        ProviderKind::Microsoft => {
            let auth = AuthClient::from_env().context("AuthClient initialisation failed")?;
            let success = auth
                .run_browser_flow(on_url)
                .await
                .context("browser sign-in failed")?;
            let tenant_id = success
                .id_token
                .as_deref()
                .and_then(extract_tenant_id)
                .unwrap_or_default();
            let graph = pidge_client::GraphClient::new(auth)?;
            let me = graph
                .me(&success.tokens.access_token)
                .await
                .context("failed to fetch /me")?;
            let email = me
                .mail
                .clone()
                .unwrap_or_else(|| me.user_principal_name.clone());
            (email, tenant_id, me.id, success.tokens)
        }
        ProviderKind::Google => {
            let cfg = google_client(&config).ok_or_else(|| {
                anyhow!(
                    "Gmail needs your own Google OAuth client: set PIDGE_GOOGLE_CLIENT_ID and \
                     PIDGE_GOOGLE_CLIENT_SECRET, or `google:` in config.yaml (see docs/gmail.md)"
                )
            })?;
            let (tokens, email) = pidge_gmail::auth::run_loopback_flow(&cfg, on_url)
                .await
                .map_err(|e| anyhow!("Google sign-in failed: {e}"))?;
            (email.clone(), String::new(), email, tokens)
        }
    };
    TokenStore::save(&email, &tokens, storage)?;
    config.add_account(Account {
        email: email.clone(),
        tenant_id,
        home_account_id,
        added_at: Utc::now(),
        storage,
        provider,
    });
    config.save()?;
    Ok(email)
}

pub async fn run(provider: ProviderKind, storage: TokenStorage) -> Result<()> {
    println!();
    println!("Adding a mailbox to pidge.");
    if matches!(storage, TokenStorage::File) {
        println!(
            "{} Tokens will be saved as a {} on disk (mode 0600).",
            "Note:".yellow().bold(),
            "plaintext file".bold()
        );
    }
    println!(
        "{}",
        match provider {
            ProviderKind::Microsoft => "A browser window will open. Personal Microsoft accounts (outlook.com, live.com, hotmail.com) and work or school Microsoft 365 accounts both work.",
            ProviderKind::Google => "A browser window will open to sign in to Google.",
        }
        .dimmed()
    );
    println!();
    let email = sign_in(provider, storage, |url| {
        println!("{} {}", "Sign in at:".bold(), url.cyan());
        println!(
            "{}",
            "(opening your browser…  Ctrl-C here to cancel)".dimmed()
        );
        let _ = open_browser(url);
    })
    .await?;
    let count = Config::load().map(|c| c.accounts.len()).unwrap_or(1);
    println!();
    println!("{} {} <{}>", "✔".green(), "Signed in as".bold(), email);
    if count > 1 {
        println!("{count} mailboxes signed in; pidge merges them.");
    }
    println!(
        "{}",
        "Next: `pidge` shows what's new; `pidge skill > ~/.claude/skills/pidge/SKILL.md` teaches your agent.".dimmed()
    );
    Ok(())
}

/// Whether `url` may be handed to the platform's URL opener: an absolute
/// http(s) URL with no control characters or double quotes. On Windows the
/// URL is placed on a `cmd.exe` command line, where a quote would end the
/// argument and let `&`, `|` or `^` start another command; a link from an
/// MCP server's reply (`pidge mcp connect`) must never get that far.
pub(crate) fn browser_url_is_safe(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && !url
            .chars()
            .any(|c| c.is_control() || c == '"' || c.is_whitespace())
}

/// Best-effort open `url` in the user's default browser. Shared with `pidge
/// mcp connect`, which reuses the same sign-in UX.
pub(crate) fn open_browser(url: &str) -> std::io::Result<()> {
    if !browser_url_is_safe(url) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to open a URL that is not a plain http(s) link",
        ));
    }
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
        // `start` is a cmd.exe built-in, not a standalone executable. The
        // URL goes on the command line in double quotes (as one raw
        // argument: Rust only quotes arguments containing whitespace, and
        // an unquoted `&` in a query string would end the command), which
        // `browser_url_is_safe` guarantees it cannot close early.
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/c", "start", ""])
            .raw_arg(format!("\"{url}\""))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::browser_url_is_safe;

    #[test]
    fn only_plain_http_links_are_opened() {
        assert!(browser_url_is_safe(
            "https://login.microsoftonline.com/x/oauth2/v2.0/authorize?client_id=a&state=b%2Bc"
        ));
        assert!(browser_url_is_safe("http://localhost:6274/oauth/callback"));
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://x.test/connect?state=a\"&calc.exe",
            "https://x.test/a b",
            "https://x.test/\x1b]0;t\x07",
            "ftp://x.test/",
            "",
        ] {
            assert!(!browser_url_is_safe(bad), "{bad:?}");
        }
    }
}
