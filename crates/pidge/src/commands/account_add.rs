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
