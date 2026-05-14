//! Helpers for attaching files to messages.
//!
//! Graph's simple POST /me/messages/{id}/attachments endpoint maxes out
//! around 3 MB total per call. Larger files need `createUploadSession` +
//! chunked uploads, which aren't wired yet — we abort early with a clear
//! error rather than letting Graph reject the request mid-flight.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use humansize::{DECIMAL, format_size};

use pidge_client::GraphClient;

/// Graph's documented limit for simple-upload attachments. Files above this
/// must use `createUploadSession`. We reject early so users get a clean
/// error rather than an opaque Graph 4xx.
pub const MAX_SIMPLE_UPLOAD_BYTES: u64 = 3 * 1024 * 1024;

/// Upload one or more files to a draft's attachments collection. On any
/// failure (file missing, too big, Graph rejection) we return early; the
/// caller decides whether to send the draft or leave it half-built. Prints
/// a one-line confirmation per attached file.
pub async fn upload_files(
    graph: &GraphClient,
    account: &str,
    message_id: &str,
    paths: &[std::path::PathBuf],
) -> Result<()> {
    for path in paths {
        upload_one(graph, account, message_id, path)
            .await
            .with_context(|| format!("attaching {}", path.display()))?;
    }
    Ok(())
}

async fn upload_one(
    graph: &GraphClient,
    account: &str,
    message_id: &str,
    path: &Path,
) -> Result<()> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("no such file: {}", path.display()))?;
    if !meta.is_file() {
        return Err(anyhow!("not a regular file: {}", path.display()));
    }
    if meta.len() > MAX_SIMPLE_UPLOAD_BYTES {
        return Err(anyhow!(
            "{} is {} — above the {} simple-upload limit. \
             Resumable (chunked) uploads aren't implemented yet.",
            path.display(),
            format_size(meta.len(), DECIMAL),
            format_size(MAX_SIMPLE_UPLOAD_BYTES, DECIMAL),
        ));
    }

    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("invalid filename: {}", path.display()))?;
    let content_type = guess_content_type(path);

    graph
        .add_attachment(account, message_id, name, content_type, &bytes)
        .await
        .context("Microsoft Graph rejected the attachment")?;
    println!(
        "  {} {} ({}, {})",
        "+".green(),
        name,
        content_type,
        format_size(bytes.len() as u64, DECIMAL),
    );
    Ok(())
}

/// Best-effort content type from file extension. Unknown extensions fall
/// back to `application/octet-stream`, which Graph accepts and Outlook
/// displays as a generic file.
pub fn guess_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("txt") | Some("log") => "text/plain",
        Some("md") | Some("markdown") => "text/markdown",
        Some("html") | Some("htm") => "text/html",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xls") => "application/vnd.ms-excel",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("zip") => "application/zip",
        Some("tar") => "application/x-tar",
        Some("gz") | Some("tgz") => "application/gzip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn known_extensions_return_specific_types() {
        assert_eq!(
            guess_content_type(&PathBuf::from("a.pdf")),
            "application/pdf"
        );
        assert_eq!(
            guess_content_type(&PathBuf::from("photo.JPG")),
            "image/jpeg"
        );
        assert_eq!(
            guess_content_type(&PathBuf::from("doc.docx")),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
    }

    #[test]
    fn unknown_extensions_fall_back_to_octet_stream() {
        assert_eq!(
            guess_content_type(&PathBuf::from("file.weirdformat")),
            "application/octet-stream"
        );
        assert_eq!(
            guess_content_type(&PathBuf::from("no-extension")),
            "application/octet-stream"
        );
    }
}
