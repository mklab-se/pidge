//! The local frontend's side of the engine: mailboxes from `config.yaml`
//! (tokens in the keychain or files), the index in the cache dir,
//! preferences next to `config.yaml`, attachments into Downloads, and the
//! detached outbox runner.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pidge_client::auth::{AuthClient, LocalBackend, TokenStore};
use pidge_client::{GraphClient, GraphProvider};
use pidge_core::provider::{Provider, ProviderKind};
use pidge_core::{Attachment, Config};
use pidge_engine::{
    AccountInfo, FilePrefs, Frontend, Host, OpError, OpResult, Prefs, PrefsStore, User,
};
use pidge_store::Store;

pub struct LocalHost {
    frontend: Frontend,
    graph: Arc<GraphClient>,
}

fn config_path(name: &str) -> Result<PathBuf> {
    Ok(pidge_core::paths::config_dir()
        .context("no config directory on this platform")?
        .join(name))
}

fn cache_path(name: &str) -> Result<PathBuf> {
    Ok(pidge_core::paths::cache_dir()
        .context("no cache directory on this platform")?
        .join(name))
}

/// Providers for every mailbox in `config.yaml`.
fn providers(config: &Config, graph: &Arc<GraphClient>) -> Vec<Arc<dyn Provider>> {
    let google = crate::commands::account_add::google_client(config);
    let http = reqwest::Client::builder()
        .user_agent(format!("pidge/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();
    let mut out: Vec<Arc<dyn Provider>> = Vec::new();
    for a in &config.accounts {
        match a.provider {
            ProviderKind::Microsoft => {
                out.push(Arc::new(GraphProvider::new(graph.clone(), a.email.clone())));
            }
            ProviderKind::Google => match &google {
                Some(cfg) => {
                    let auth = pidge_gmail::GoogleAuth::new(
                        http.clone(),
                        cfg.clone(),
                        Arc::new(LocalBackend),
                        a.email.clone(),
                    );
                    out.push(Arc::new(pidge_gmail::GmailProvider::new(auth)));
                }
                None => tracing::warn!(
                    account = %a.email,
                    "skipping a Gmail mailbox: no Google OAuth client configured (docs/gmail.md)"
                ),
            },
        }
    }
    out
}

/// The local user, ready to run operations.
pub async fn open_user(frontend: Frontend) -> Result<Arc<User>> {
    let config = Config::load()?;
    let auth = AuthClient::from_env().context("could not set up Microsoft sign-in")?;
    let graph = Arc::new(GraphClient::new(auth)?);
    let store = Arc::new(Store::open(&cache_path("index.db")?)?);
    let prefs = Arc::new(FilePrefs::new(config_path("prefs.json")?));
    seed_prefs(prefs.as_ref(), &config).await;
    let user = User::new(
        "local",
        store,
        prefs,
        providers(&config, &graph),
        Arc::new(LocalHost { frontend, graph }),
    );
    if let Some(ai) = crate::commands::ai::client() {
        user.set_ai(Some(Arc::new(ai)));
    }
    Ok(user)
}

/// The first time: carry over the default account and trusted senders.
async fn seed_prefs(store: &FilePrefs, config: &Config) {
    if let Ok(None) = store.load().await {
        let p = Prefs {
            default_sender: config.defaults.send.clone(),
            vips: config.trusted_senders.clone(),
            ..Default::default()
        };
        let _ = store.save(&p).await;
    }
}

fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "attachment".into()
    } else {
        cleaned.chars().take(150).collect()
    }
}

#[async_trait]
impl Host for LocalHost {
    fn frontend(&self) -> Frontend {
        self.frontend
    }

    fn default_timezone(&self) -> String {
        iana_time_zone::get_timezone().unwrap_or_else(|_| "Europe/Stockholm".into())
    }

    async fn accounts(&self) -> Vec<AccountInfo> {
        let config = Config::load().unwrap_or_default();
        let mut out = Vec::new();
        for a in &config.accounts {
            let status = match TokenStore::load(&a.email, a.storage) {
                Ok(Some(_)) => "ok",
                _ => "needs sign-in (pidge account add)",
            };
            out.push(AccountInfo {
                address: a.email.clone(),
                provider: a.provider,
                status: status.into(),
            });
        }
        out
    }

    async fn connect(&self, provider: ProviderKind, _email: Option<&str>) -> OpResult<String> {
        let email = crate::commands::account_add::sign_in(provider, Default::default(), |url| {
            eprintln!("Sign in at: {url}");
            let _ = crate::commands::account_add::open_browser(url);
        })
        .await
        .map_err(|e| OpError::new(pidge_engine::ErrorKind::Provider, e.to_string()))?;
        Ok(format!(
            "Signed in as {email}. It is used from the next request (restart the MCP server if it does not show)."
        ))
    }

    async fn disconnect(&self, address: &str) -> OpResult<String> {
        let mut config = Config::load().map_err(|e| OpError::internal(e.to_string()))?;
        let storage = config.find(address).map(|a| a.storage).unwrap_or_default();
        let _ = TokenStore::delete(address, storage);
        config.remove_account(address);
        config
            .save()
            .map_err(|e| OpError::internal(e.to_string()))?;
        Ok(format!("Signed out of {address}."))
    }

    async fn ai_setup(&self) -> OpResult<String> {
        Ok("The user runs `pidge ai config` in a terminal to set up their own AI key (TypeSafe Jev, OpenAI or Anthropic through Ailloy), then `pidge ai enable`.".into())
    }

    async fn deliver_attachment(
        &self,
        _account: &str,
        _message_id: &str,
        attachment: &Attachment,
        bytes: Vec<u8>,
    ) -> OpResult<String> {
        let dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .ok_or_else(|| OpError::internal("no Downloads folder"))?;
        let name = safe_file_name(&attachment.name);
        let mut path = dir.join(&name);
        let mut n = 1;
        while path.exists() {
            let (stem, ext) = match name.rsplit_once('.') {
                Some((s, e)) => (s.to_string(), format!(".{e}")),
                None => (name.clone(), String::new()),
            };
            path = dir.join(format!("{stem} ({n}){ext}"));
            n += 1;
        }
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|e| OpError::internal(e.to_string()))?;
        Ok(format!("Saved to {}", path.display()))
    }

    /// `markitdown`, when it is installed.
    async fn convert_document(
        &self,
        name: &str,
        _content_type: &str,
        bytes: Vec<u8>,
    ) -> Option<Result<String, String>> {
        let exe = which("markitdown")?;
        let dir = std::env::temp_dir().join(format!("pidge-convert-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let path = dir.join(safe_file_name(name));
        if let Err(e) = tokio::fs::write(&path, bytes).await {
            return Some(Err(e.to_string()));
        }
        let run = tokio::process::Command::new(exe)
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let result = match tokio::time::timeout(Duration::from_secs(30), run).await {
            Ok(Ok(out)) if out.status.success() => {
                Ok(String::from_utf8_lossy(&out.stdout).to_string())
            }
            Ok(Ok(_)) => Err("markitdown could not read it".into()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("conversion took too long".into()),
        };
        let _ = tokio::fs::remove_dir_all(&dir).await;
        Some(result)
    }

    async fn one_click_unsubscribe(&self, url: &str) -> Result<(), String> {
        self.graph
            .unsubscribe_one_click(url)
            .await
            .map_err(|e| e.to_string())
    }

    /// A short-lived CLI cannot hold a send: start a detached runner.
    fn outbox_due(&self, _at: DateTime<Utc>) {
        if self.frontend != Frontend::Cli {
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.args(["outbox", "run"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        if let Err(e) = cmd.spawn() {
            tracing::warn!(error = %e, "could not start the outbox runner; held mail goes out on the next pidge command");
        }
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
        let exe = dir.join(format!("{name}.exe"));
        exe.is_file().then_some(exe)
    })
}

/// `pidge mcp serve`: the engine's tools over stdio.
pub async fn serve_stdio() -> Result<()> {
    use rmcp::ServiceExt;
    let user = open_user(Frontend::Mcp).await?;
    let server = pidge_engine::mcp::PidgeServer::new(Local(user));
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .context("could not start the MCP server on stdio")?;
    running.waiting().await?;
    Ok(())
}

struct Local(Arc<User>);

#[async_trait]
impl pidge_engine::mcp::UserSource for Local {
    async fn user(
        &self,
        _ctx: &rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<Arc<User>, String> {
        // Pick up mailboxes added since the server started.
        if let Ok(config) = Config::load()
            && config.accounts.len() != self.0.providers().len()
            && let Ok(auth) = AuthClient::from_env()
            && let Ok(graph) = GraphClient::new(auth)
        {
            self.0.set_providers(providers(&config, &Arc::new(graph)));
        }
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::safe_file_name;

    #[test]
    fn attachment_names_cannot_escape_downloads() {
        assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_file_name("..\\x.pdf"), "_x.pdf");
        assert_eq!(safe_file_name(""), "attachment");
        assert_eq!(safe_file_name("Faktura okt.pdf"), "Faktura okt.pdf");
    }
}
