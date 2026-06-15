//! `pidge mail folders` — list the mail folders in each signed-in account,
//! with message counts. Handy for confirming where `mail move` will (or did)
//! file things, and for discovering a folder's exact display name.

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use futures::future::BoxFuture;
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

/// Split a folder path on `/` into trimmed, non-empty segments. A path like
/// `"Kvitton/MKLab"` addresses the child folder `MKLab` under the top-level
/// folder `Kvitton`. Pure so the parsing is unit-testable.
pub(crate) fn split_folder_path(path: &str) -> Vec<&str> {
    path.split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Resolve a (possibly nested) folder `path` to a folder ID for `account`,
/// creating any missing level along the way. `"Kvitton/MKLab"` ensures the
/// top-level `Kvitton` then its child `MKLab`. Returns the leaf folder's ID
/// and whether any level had to be created. Idempotent.
pub(crate) async fn ensure_folder(
    graph: &GraphClient,
    account: &str,
    path: &str,
) -> Result<(String, bool)> {
    let segments = split_folder_path(path);
    if segments.is_empty() {
        return Err(anyhow!("folder name must be non-empty."));
    }
    let mut created_any = false;

    // Top level.
    let top = graph
        .list_mail_folders(account)
        .await
        .with_context(|| format!("listing folders for {account}"))?;
    let mut current_id = match find_folder_id(&top, segments[0]) {
        Some(id) => id.to_string(),
        None => {
            created_any = true;
            graph
                .create_mail_folder(account, segments[0])
                .await
                .with_context(|| format!("creating folder '{}' in {account}", segments[0]))?
                .id
        }
    };

    // Descend into / create each child level.
    for seg in &segments[1..] {
        let children = graph
            .list_child_folders(account, &current_id)
            .await
            .with_context(|| format!("listing child folders of '{seg}' in {account}"))?;
        current_id = match find_folder_id(&children, seg) {
            Some(id) => id.to_string(),
            None => {
                created_any = true;
                graph
                    .create_child_folder(account, &current_id, seg)
                    .await
                    .with_context(|| format!("creating folder '{seg}' in {account}"))?
                    .id
            }
        };
    }

    Ok((current_id, created_any))
}

/// Resolve a (possibly nested) folder `path` to its leaf folder ID for
/// `account` **without creating anything**. Returns `None` if any level is
/// missing. Used by `mail rmdir`.
pub(crate) async fn resolve_folder_path(
    graph: &GraphClient,
    account: &str,
    path: &str,
) -> Result<Option<String>> {
    let segments = split_folder_path(path);
    if segments.is_empty() {
        return Ok(None);
    }
    let top = graph
        .list_mail_folders(account)
        .await
        .with_context(|| format!("listing folders for {account}"))?;
    let Some(id) = find_folder_id(&top, segments[0]) else {
        return Ok(None);
    };
    let mut current_id = id.to_string();
    for seg in &segments[1..] {
        let children = graph
            .list_child_folders(account, &current_id)
            .await
            .with_context(|| format!("listing child folders of '{seg}' in {account}"))?;
        match find_folder_id(&children, seg) {
            Some(id) => current_id = id.to_string(),
            None => return Ok(None),
        }
    }
    Ok(Some(current_id))
}

/// `pidge mail rmdir <path> -y` — delete a folder (and its contents, which
/// Outlook moves to Deleted Items) in each target account. Requires `-y`
/// since it is destructive. A path that doesn't exist in an account is
/// reported and skipped, not an error.
pub async fn rmdir(name: String, account_filter: Vec<String>, yes: bool) -> Result<()> {
    if name.trim().is_empty() {
        return Err(anyhow!("folder name must be non-empty."));
    }
    if !yes {
        return Err(anyhow!(
            "Deleting a folder is destructive (its contents move to Deleted \
             Items). Re-run with `-y` to confirm."
        ));
    }
    let target_emails = resolve_target_emails(account_filter)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    for email in &target_emails {
        match resolve_folder_path(&graph, email, &name).await? {
            Some(folder_id) => {
                graph
                    .delete_mail_folder(email, &folder_id)
                    .await
                    .with_context(|| format!("deleting folder '{name}' in {email}"))?;
                println!(
                    "{} Deleted folder {} in {}.",
                    "✔".green(),
                    name.cyan(),
                    email.dimmed()
                );
            }
            None => {
                println!(
                    "{} No folder {} in {} — skipped.",
                    "·".dimmed(),
                    name.cyan(),
                    email.dimmed()
                );
            }
        }
    }
    Ok(())
}

/// `pidge mail mkdir <name>` — create a folder (or nested path like
/// `Kvitton/MKLab`) in each target account, idempotently. Existing levels
/// are left alone.
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

    let mut accounts_out: Vec<AccountFolders> = Vec::new();
    let mut had_success = false;
    for email in &target_emails {
        match graph.list_mail_folders(email).await {
            Ok(folders) => match build_subtree(&graph, email, folders).await {
                Ok(tree) => {
                    had_success = true;
                    accounts_out.push(AccountFolders {
                        account: email.clone(),
                        folders: tree,
                    });
                }
                Err(e) => eprintln!("{} {email}: {e}", "WARNING:".yellow().bold()),
            },
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
        print_tree(&acct.folders, 1);
    }
    Ok(())
}

/// Recursively turn a folder list into `FolderOut` nodes, fetching each
/// folder's children when `childFolderCount` says it has any. Sorted
/// case-insensitively by name at every level.
fn build_subtree<'a>(
    graph: &'a GraphClient,
    account: &'a str,
    mut folders: Vec<MailFolder>,
) -> BoxFuture<'a, Result<Vec<FolderOut>>> {
    Box::pin(async move {
        folders.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        });
        let mut out = Vec::with_capacity(folders.len());
        for f in folders {
            let children = if f.child_folder_count.unwrap_or(0) > 0 {
                let kids = graph
                    .list_child_folders(account, &f.id)
                    .await
                    .with_context(|| {
                        format!("listing child folders of '{}' in {account}", f.display_name)
                    })?;
                build_subtree(graph, account, kids).await?
            } else {
                Vec::new()
            };
            out.push(FolderOut {
                name: f.display_name,
                total: f.total_item_count.unwrap_or(0),
                unread: f.unread_item_count.unwrap_or(0),
                children,
            });
        }
        Ok(out)
    })
}

/// Print a folder tree with two-space indentation per depth level.
fn print_tree(nodes: &[FolderOut], depth: usize) {
    let indent = "  ".repeat(depth);
    for n in nodes {
        let count = if n.unread > 0 {
            format!("{} ({} unread)", n.total, n.unread)
        } else {
            n.total.to_string()
        };
        println!("{indent}{}  {}", n.name.yellow(), count.dimmed());
        print_tree(&n.children, depth + 1);
    }
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<FolderOut>,
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
            child_folder_count: None,
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

    #[test]
    fn split_folder_path_trims_and_drops_empties() {
        assert_eq!(split_folder_path("Kvitton/MKLab"), vec!["Kvitton", "MKLab"]);
        assert_eq!(split_folder_path("  Biljetter  "), vec!["Biljetter"]);
        assert_eq!(
            split_folder_path("Kvitton / / Privata /"),
            vec!["Kvitton", "Privata"]
        );
        assert!(split_folder_path("  /  ").is_empty());
    }
}
