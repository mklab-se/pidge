//! `pidge mail folders` — list the mail folders in each signed-in account,
//! with message counts. Handy for confirming where `mail move` will (or did)
//! file things, and for discovering a folder's exact display name.

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use futures::future::join_all;
use serde::Serialize;

use pidge_client::{AuthClient, ClientError, GraphClient, MailFolder};
use pidge_core::Config;

/// Find an existing folder by display name (case-insensitive), returning its
/// Graph ID. Pure so it's unit-testable without a live mailbox.
pub(crate) fn find_folder_id<'a>(folders: &'a [MailFolder], name: &str) -> Option<&'a str> {
    folders
        .iter()
        .find(|f| f.display_name.eq_ignore_ascii_case(name))
        .map(|f| f.id.as_str())
}

/// Resolve `name` to a folder ID for `account`, creating the folder if no
/// case-insensitive match exists. Returns the ID and whether it was created
/// (so the caller can tell the user a new folder appeared). Idempotent —
/// safe to call repeatedly.
pub(crate) async fn ensure_folder(
    graph: &GraphClient,
    account: &str,
    name: &str,
) -> Result<(String, bool)> {
    let folders = graph
        .list_mail_folders(account)
        .await
        .with_context(|| format!("listing folders for {account}"))?;
    if let Some(id) = find_folder_id(&folders, name) {
        return Ok((id.to_string(), false));
    }
    let created = graph
        .create_mail_folder(account, name)
        .await
        .with_context(|| format!("creating folder '{name}' in {account}"))?;
    Ok((created.id, true))
}

/// `pidge mail mkdir <name>` — create a top-level folder in each target
/// account (idempotent; an existing folder of the same name is left alone).
pub async fn mkdir(name: String, account_filter: Vec<String>) -> Result<()> {
    if name.trim().is_empty() {
        return Err(anyhow!("folder name must be non-empty."));
    }
    let target_emails = resolve_target_emails(account_filter)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    for email in &target_emails {
        let (_, created) = ensure_folder(&graph, email, &name).await?;
        if created {
            println!(
                "{} Created folder {} in {}.",
                "✔".green(),
                name.cyan(),
                email.dimmed()
            );
        } else {
            println!(
                "{} Folder {} already exists in {}.",
                "·".dimmed(),
                name.cyan(),
                email.dimmed()
            );
        }
    }
    Ok(())
}

/// Resolve an optional account filter to the concrete list of e-mails to act
/// on, erroring if a named account isn't signed in. Empty filter → all
/// signed-in accounts.
fn resolve_target_emails(account_filter: Vec<String>) -> Result<Vec<String>> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge account add` to add one."
        ));
    }
    if account_filter.is_empty() {
        Ok(config.accounts.iter().map(|a| a.email.clone()).collect())
    } else {
        for f in &account_filter {
            if config.find(f).is_none() {
                return Err(anyhow!("not signed in to {f}"));
            }
        }
        Ok(account_filter)
    }
}

pub async fn run(account_filter: Vec<String>, json: bool) -> Result<()> {
    let target_emails = resolve_target_emails(account_filter)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let futures = target_emails.iter().map(|email| {
        let graph = &graph;
        let e = email.clone();
        async move {
            let result = graph.list_mail_folders(&e).await;
            (e, result)
        }
    });
    let results = join_all(futures).await;

    let mut accounts_out: Vec<AccountFolders> = Vec::new();
    let mut had_success = false;
    for (email, result) in results {
        match result {
            Ok(mut folders) => {
                had_success = true;
                folders.sort_by(|a, b| {
                    a.display_name
                        .to_lowercase()
                        .cmp(&b.display_name.to_lowercase())
                });
                accounts_out.push(AccountFolders {
                    account: email,
                    folders: folders
                        .into_iter()
                        .map(|f| FolderOut {
                            name: f.display_name,
                            total: f.total_item_count.unwrap_or(0),
                            unread: f.unread_item_count.unwrap_or(0),
                        })
                        .collect(),
                });
            }
            Err(ClientError::SessionExpired { email: e }) => {
                eprintln!(
                    "{} {e}: session expired, run `pidge account add`",
                    "WARNING:".yellow().bold()
                );
            }
            Err(e) => {
                eprintln!("{} {email}: {e}", "WARNING:".yellow().bold());
            }
        }
    }

    if !had_success {
        return Err(anyhow!("All accounts failed."));
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&accounts_out)?);
        return Ok(());
    }

    let multi = accounts_out.len() > 1;
    for (i, acct) in accounts_out.iter().enumerate() {
        if multi {
            if i > 0 {
                println!();
            }
            println!("{}", acct.account.cyan().bold());
        }
        for f in &acct.folders {
            let count = if f.unread > 0 {
                format!("{} ({} unread)", f.total, f.unread)
            } else {
                f.total.to_string()
            };
            println!("  {}  {}", f.name.yellow(), count.dimmed());
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct AccountFolders {
    account: String,
    folders: Vec<FolderOut>,
}

#[derive(Serialize)]
struct FolderOut {
    name: String,
    total: u64,
    unread: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, name: &str) -> MailFolder {
        MailFolder {
            id: id.to_string(),
            display_name: name.to_string(),
            total_item_count: None,
            unread_item_count: None,
        }
    }

    #[test]
    fn find_folder_id_matches_case_insensitively() {
        let folders = vec![folder("F1", "Biljetter"), folder("F2", "Kvitton")];
        assert_eq!(find_folder_id(&folders, "biljetter"), Some("F1"));
        assert_eq!(find_folder_id(&folders, "KVITTON"), Some("F2"));
    }

    #[test]
    fn find_folder_id_returns_none_when_absent() {
        let folders = vec![folder("F1", "Biljetter")];
        assert_eq!(find_folder_id(&folders, "Resor"), None);
    }
}
