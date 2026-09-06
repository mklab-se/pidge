//! `pidge mail attachments {list,save}` — list and download a received
//! message's file attachments.
//!
//! The Graph client already exposes `list_attachments` (metadata) and
//! `get_attachment_bytes` (decoded content); this command resolves a message
//! fragment, picks which attachments to write, and saves them to disk. The
//! selection and destination logic is factored into pure helpers so it can be
//! unit-tested without a live mailbox.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use humansize::{DECIMAL, format_size};

use pidge_client::{AuthClient, GraphClient};
use pidge_core::Attachment;

use crate::cli::MailAttachmentCommands;
use crate::commands::mail_fragment;

pub async fn run(command: MailAttachmentCommands, json: bool) -> Result<()> {
    match command {
        MailAttachmentCommands::List {
            fragment,
            include_inline,
        } => list(fragment, include_inline, json).await,
        MailAttachmentCommands::Save {
            fragment,
            name,
            out,
            include_inline,
            force,
        } => save(fragment, name, out, include_inline, force).await,
    }
}

async fn list(fragment: String, include_inline: bool, json: bool) -> Result<()> {
    let (_, message_ref) = mail_fragment::resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let attachments = graph
        .list_attachments(&message_ref.account, &message_ref.graph_id)
        .await?;
    let shown: Vec<&Attachment> = attachments
        .iter()
        .filter(|a| include_inline || !a.is_inline)
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&shown)?);
        return Ok(());
    }

    if shown.is_empty() {
        println!("No attachments.");
        return Ok(());
    }

    let mut table = Table::new();
    table.load_style(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["NAME", "SIZE", "TYPE"]);
    for a in shown {
        let name = if a.is_inline {
            format!("{} {}", a.name, "(inline)".dimmed())
        } else {
            a.name.clone()
        };
        table.add_row(vec![
            name,
            format_size(a.size_bytes, DECIMAL),
            a.content_type.clone(),
        ]);
    }
    println!("{table}");
    Ok(())
}

async fn save(
    fragment: String,
    name: Option<String>,
    out: Option<PathBuf>,
    include_inline: bool,
    force: bool,
) -> Result<()> {
    let (_, message_ref) = mail_fragment::resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let attachments = graph
        .list_attachments(&message_ref.account, &message_ref.graph_id)
        .await?;

    let selected = select_attachments(&attachments, name.as_deref(), include_inline)?;

    let out_is_existing_dir = out.as_deref().map(Path::is_dir).unwrap_or(false);
    let dest = resolve_destination(
        out,
        default_download_dir(),
        out_is_existing_dir,
        selected.len(),
    )?;

    for att in &selected {
        let target = final_path(&dest, &att.name);
        guard_overwrite(&target, target.exists(), force)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let bytes = graph
            .get_attachment_bytes(&message_ref.account, &message_ref.graph_id, &att.id)
            .await
            .with_context(|| format!("downloading {}", att.name))?;
        std::fs::write(&target, &bytes).with_context(|| format!("writing {}", target.display()))?;
        println!(
            "  {} {} → {} ({})",
            "+".green(),
            att.name,
            target.display(),
            format_size(bytes.len() as u64, DECIMAL),
        );
    }
    Ok(())
}

/// The directory `save` writes into when `--out` is omitted: the user's
/// Downloads folder, falling back to home, then the current directory.
fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Pick which attachments to act on.
///
/// Inline attachments (embedded images, signatures) are excluded unless
/// `include_inline` is set. When `name` is given it is matched case-insensitively
/// as a substring of the filename: zero matches and (to force an explicit
/// choice) more than one match are both errors. With no `name`, every attachment
/// in the pool is selected.
fn select_attachments(
    attachments: &[Attachment],
    name: Option<&str>,
    include_inline: bool,
) -> Result<Vec<Attachment>> {
    let pool: Vec<Attachment> = attachments
        .iter()
        .filter(|a| include_inline || !a.is_inline)
        .cloned()
        .collect();

    let Some(name) = name else {
        if pool.is_empty() {
            return Err(anyhow!("Message has no downloadable attachments."));
        }
        return Ok(pool);
    };

    let needle = name.to_lowercase();
    let matches: Vec<Attachment> = pool
        .iter()
        .filter(|a| a.name.to_lowercase().contains(&needle))
        .cloned()
        .collect();

    match matches.len() {
        0 => Err(anyhow!(
            "No attachment matches '{name}'. Available: {}.",
            attachment_names(&pool)
        )),
        1 => Ok(matches),
        _ => Err(anyhow!(
            "'{name}' matches several attachments: {}. Narrow the name.",
            attachment_names(&matches)
        )),
    }
}

/// Comma-joined attachment filenames, for error messages.
fn attachment_names(atts: &[Attachment]) -> String {
    atts.iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where saved files should land.
#[derive(Debug, PartialEq, Eq)]
enum Destination {
    /// Write each attachment into this directory under its own filename.
    IntoDir(PathBuf),
    /// Write the single selected attachment to exactly this file path.
    AsFile(PathBuf),
}

/// Resolve the `--out` argument into a [`Destination`].
///
/// `None` → the default directory. A path that is an existing directory, or
/// that ends in the platform separator, is treated as a directory. Otherwise it
/// is a target file path — which is only valid when a single attachment is
/// selected; selecting several would collapse them onto one filename, so that
/// is rejected.
fn resolve_destination(
    out: Option<PathBuf>,
    default_dir: PathBuf,
    out_is_existing_dir: bool,
    selected_count: usize,
) -> Result<Destination> {
    let Some(path) = out else {
        return Ok(Destination::IntoDir(default_dir));
    };

    if out_is_existing_dir || has_trailing_separator(&path) {
        return Ok(Destination::IntoDir(path));
    }

    if selected_count > 1 {
        return Err(anyhow!(
            "--out is a file path but {selected_count} attachments are selected. \
             Pass a directory, or narrow to one attachment by name."
        ));
    }

    Ok(Destination::AsFile(path))
}

/// True if the path's text ends with the platform path separator (a user's way
/// of saying "this is a directory" even when it doesn't exist yet).
fn has_trailing_separator(path: &Path) -> bool {
    path.as_os_str()
        .to_string_lossy()
        .ends_with(std::path::MAIN_SEPARATOR)
}

/// Compute the on-disk path for one attachment given the resolved destination.
/// For a directory destination the attachment's filename is sanitized to its
/// final path component, guarding against a hostile name like `../../etc/x`.
fn final_path(dest: &Destination, original_name: &str) -> PathBuf {
    match dest {
        Destination::AsFile(path) => path.clone(),
        Destination::IntoDir(dir) => {
            let leaf = Path::new(original_name)
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("attachment"));
            dir.join(leaf)
        }
    }
}

/// Reject writing over an existing file unless the user passed `--force`.
/// Existence is checked by the caller so this stays pure and testable.
fn guard_overwrite(path: &Path, exists: bool, force: bool) -> Result<()> {
    if exists && !force {
        return Err(anyhow!(
            "{} already exists. Pass --force to overwrite.",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(name: &str, inline: bool) -> Attachment {
        Attachment {
            id: format!("id-{name}"),
            name: name.to_string(),
            content_type: "application/octet-stream".to_string(),
            size_bytes: 10,
            is_inline: inline,
            content_id: None,
        }
    }

    #[test]
    fn no_name_excludes_inline_by_default() {
        let atts = vec![att("report.pdf", false), att("logo.png", true)];
        let picked = select_attachments(&atts, None, false).unwrap();
        let names: Vec<_> = picked.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["report.pdf"]);
    }

    #[test]
    fn no_name_with_include_inline_keeps_inline() {
        let atts = vec![att("report.pdf", false), att("logo.png", true)];
        let picked = select_attachments(&atts, None, true).unwrap();
        assert_eq!(picked.len(), 2);
    }

    #[test]
    fn name_substring_matches_case_insensitively() {
        let atts = vec![att("Quarterly-Report.pdf", false), att("photo.jpg", false)];
        let picked = select_attachments(&atts, Some("report"), false).unwrap();
        let names: Vec<_> = picked.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Quarterly-Report.pdf"]);
    }

    #[test]
    fn name_matching_nothing_is_an_error() {
        let atts = vec![att("report.pdf", false)];
        let err = select_attachments(&atts, Some("invoice"), false).unwrap_err();
        assert!(err.to_string().contains("invoice"));
        assert!(err.to_string().contains("report.pdf"));
    }

    #[test]
    fn name_matching_several_is_an_error() {
        let atts = vec![att("report-q1.pdf", false), att("report-q2.pdf", false)];
        let err = select_attachments(&atts, Some("report"), false).unwrap_err();
        assert!(err.to_string().contains("report-q1.pdf"));
        assert!(err.to_string().contains("report-q2.pdf"));
    }

    #[test]
    fn empty_pool_is_an_error() {
        let atts = vec![att("logo.png", true)];
        let err = select_attachments(&atts, None, false).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("no"));
    }

    #[test]
    fn no_out_uses_default_dir() {
        let dest =
            resolve_destination(None, PathBuf::from("/home/jane/Downloads"), false, 3).unwrap();
        assert_eq!(
            dest,
            Destination::IntoDir(PathBuf::from("/home/jane/Downloads"))
        );
    }

    #[test]
    fn out_file_path_with_single_selection_is_a_file() {
        let dest = resolve_destination(
            Some(PathBuf::from("/tmp/renamed.pdf")),
            PathBuf::from("/dl"),
            false,
            1,
        )
        .unwrap();
        assert_eq!(dest, Destination::AsFile(PathBuf::from("/tmp/renamed.pdf")));
    }

    #[test]
    fn out_file_path_with_multiple_selection_is_an_error() {
        let err = resolve_destination(
            Some(PathBuf::from("/tmp/renamed.pdf")),
            PathBuf::from("/dl"),
            false,
            2,
        )
        .unwrap_err();
        assert!(err.to_string().contains("2"));
    }

    #[test]
    fn out_existing_dir_is_a_directory() {
        let dest = resolve_destination(
            Some(PathBuf::from("/tmp/some-dir")),
            PathBuf::from("/dl"),
            true,
            2,
        )
        .unwrap();
        assert_eq!(dest, Destination::IntoDir(PathBuf::from("/tmp/some-dir")));
    }

    #[test]
    fn out_trailing_separator_is_a_directory_even_if_missing() {
        let with_sep = format!("/tmp/new-dir{}", std::path::MAIN_SEPARATOR);
        let dest = resolve_destination(
            Some(PathBuf::from(&with_sep)),
            PathBuf::from("/dl"),
            false,
            2,
        )
        .unwrap();
        assert_eq!(dest, Destination::IntoDir(PathBuf::from(&with_sep)));
    }

    #[test]
    fn final_path_into_dir_joins_sanitized_filename() {
        let dest = Destination::IntoDir(PathBuf::from("/dl"));
        assert_eq!(
            final_path(&dest, "report.pdf"),
            PathBuf::from("/dl/report.pdf")
        );
        // A hostile name collapses to its final component.
        assert_eq!(
            final_path(&dest, "../../etc/passwd"),
            PathBuf::from("/dl/passwd")
        );
    }

    #[test]
    fn existing_target_without_force_is_rejected() {
        let err = guard_overwrite(Path::new("/dl/report.pdf"), true, false).unwrap_err();
        assert!(err.to_string().contains("report.pdf"));
        assert!(err.to_string().contains("force"));
    }

    #[test]
    fn existing_target_with_force_is_allowed() {
        assert!(guard_overwrite(Path::new("/dl/report.pdf"), true, true).is_ok());
    }

    #[test]
    fn missing_target_is_allowed() {
        assert!(guard_overwrite(Path::new("/dl/report.pdf"), false, false).is_ok());
    }

    #[test]
    fn final_path_as_file_returns_the_file_path() {
        let dest = Destination::AsFile(PathBuf::from("/tmp/renamed.pdf"));
        assert_eq!(
            final_path(&dest, "original.pdf"),
            PathBuf::from("/tmp/renamed.pdf")
        );
    }
}
