//! `GET /dl/{token}`: serves one attachment to whoever holds a
//! `mail_attachment` link (spec §1.10). The signed token is the only
//! credential, so the route sits outside the bearer layer. The token names
//! the user and mailbox only by hash; the route resolves the user against
//! the allowlist, charges their hourly download budget, and finds the
//! mailbox among those they still own. Every refusal looks the same: a
//! neutral 404.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::oauth::pages;
use crate::state::SharedState;
use crate::users::user_hash;

const FILENAME_MAX: usize = 100;

pub async fn download(State(state): State<SharedState>, Path(token): Path<String>) -> Response {
    let Ok(claims) = state.signer.verify_download(&token) else {
        tracing::info!(outcome = "invalid link", "download refused");
        return pages::link_unavailable();
    };
    let user = claims.uh.as_str();
    let refuse = |outcome: &str| {
        tracing::info!(%user, outcome, "download refused");
        pages::link_unavailable()
    };

    // The token names people only by hash: find the allowlisted user…
    let Some(signin) = state
        .config
        .allowed_emails
        .iter()
        .find(|e| user_hash(e) == claims.uh)
        .cloned()
    else {
        return refuse("user not allowed");
    };
    if !state.reserve_download(&signin) {
        return refuse("rate limited");
    }
    // …and the mailbox among the ones they still own.
    let account = match state.users.load(&signin).await {
        Ok(Some(record)) => record
            .mailboxes
            .into_iter()
            .find(|m| user_hash(m) == claims.mh),
        Ok(None) => None,
        Err(_) => return refuse("user store unavailable"),
    };
    let Some(account) = account else {
        return refuse("mailbox not owned");
    };
    // The error is not logged: Graph failures can name the mailbox.
    let bytes = match state
        .graph
        .get_attachment_bytes(&account, &claims.message_id, &claims.attachment_id)
        .await
    {
        Ok(bytes) => bytes,
        Err(_) => return refuse("graph fetch failed"),
    };

    tracing::info!(%user, outcome = "served", "download");
    let content_type = HeaderValue::from_str(claims.content_type.trim())
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"));
    let disposition = format!(
        r#"attachment; filename="{}""#,
        safe_filename(&claims.filename)
    );
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&disposition).expect("sanitised filename is a valid header"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            // Even if a browser renders it, the file gets no origin or scripts.
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox"),
            ),
        ],
        Body::from(bytes),
    )
        .into_response()
}

/// `name` as a `Content-Disposition` filename: ASCII letters, digits and
/// `._-` only (anything else becomes `_`), no leading dots, at most
/// [`FILENAME_MAX`] characters with a short extension kept when cutting.
fn safe_filename(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut name = mapped.trim_start_matches('.').to_string();
    if name.is_empty() {
        return "attachment".to_string();
    }
    if name.len() > FILENAME_MAX {
        // All ASCII now, so byte offsets are character offsets.
        name = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && ext.len() <= 16 => {
                format!("{}.{ext}", &stem[..FILENAME_MAX - 1 - ext.len()])
            }
            _ => name[..FILENAME_MAX].to_string(),
        };
    }
    name
}

#[cfg(test)]
pub(crate) mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use super::*;
    use crate::app::build_router;
    use crate::state::DOWNLOADS_PER_HOUR;
    use crate::test_support::{LogCapture, assert_no_address};
    use crate::tools::attachments::tests::{JANE, WORK, mount_bytes};
    use crate::tools::tests::ToolHarness;

    const PDF: &[u8] = b"%PDF-1.7 fake";

    fn attachment(name: &str) -> pidge_core::Attachment {
        pidge_core::Attachment {
            id: "A1".into(),
            name: name.into(),
            content_type: "application/pdf".into(),
            size_bytes: PDF.len() as u64,
            is_inline: false,
            content_id: None,
        }
    }

    fn token(h: &ToolHarness, sub: &str, account: &str, name: &str) -> String {
        h.state
            .signer
            .issue_download(sub, account, "M1", &attachment(name))
            .unwrap()
    }

    pub(crate) async fn get(
        h: &ToolHarness,
        path: &str,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let app = build_router(h.state.clone(), CancellationToken::new());
        let resp = app
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, headers, body)
    }

    fn assert_refused(status: StatusCode, body: &[u8]) {
        assert_eq!(status, StatusCode::NOT_FOUND);
        let page = String::from_utf8_lossy(body);
        assert!(!page.contains("PDF"), "leaked bytes: {page}");
        assert!(page.contains("invalid or has expired"), "{page}");
    }

    #[tokio::test]
    async fn a_valid_link_streams_the_bytes_with_safe_headers_and_logs_no_address() {
        let (logs, _guard) = LogCapture::start();
        let h = ToolHarness::new(&[JANE, WORK]).await;
        mount_bytes(&h, WORK, "M1", "A1", PDF, 1).await;
        let t = token(&h, JANE, WORK, "Q3 Report (final) – ö.pdf");

        let (status, headers, body) = get(&h, &format!("/dl/{t}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, PDF);
        assert_eq!(headers[header::CONTENT_TYPE], "application/pdf");
        assert_eq!(
            headers[header::CONTENT_DISPOSITION],
            r#"attachment; filename="Q3_Report__final_____.pdf""#
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], "sandbox");

        let logged = logs.text();
        assert!(logged.contains("served"), "{logged}");
        // The request span names the route but never the token.
        assert!(logged.contains("/dl/<redacted>"), "{logged}");
        assert!(!logged.contains(&t), "token logged: {logged}");
        assert_no_address("logs", &logged);
    }

    #[tokio::test]
    async fn a_link_for_a_user_who_does_not_own_the_mailbox_is_refused() {
        let h = ToolHarness::new(&[JANE]).await;
        mount_bytes(&h, JANE, "M1", "A1", PDF, 0).await;
        // Mallory has no record, so owns nothing (and isn't allowlisted).
        let t = token(&h, "mallory@example.com", JANE, "a.pdf");
        let (status, _, body) = get(&h, &format!("/dl/{t}")).await;
        assert_refused(status, &body);

        // Jane is allowlisted but WORK isn't hers.
        let t = token(&h, JANE, WORK, "a.pdf");
        let (status, _, body) = get(&h, &format!("/dl/{t}")).await;
        assert_refused(status, &body);
    }

    #[tokio::test]
    async fn expired_and_tampered_links_are_refused() {
        let h = ToolHarness::new(&[JANE]).await;
        mount_bytes(&h, JANE, "M1", "A1", PDF, 0).await;
        let expired = h
            .state
            .signer
            .issue_download_with_ttl(
                JANE,
                JANE,
                "M1",
                &attachment("a.pdf"),
                chrono::Duration::minutes(-5),
            )
            .unwrap();
        let (status, _, body) = get(&h, &format!("/dl/{expired}")).await;
        assert_refused(status, &body);

        let good = token(&h, JANE, JANE, "a.pdf");
        let (payload_at, _) = good.match_indices('.').next().unwrap();
        let mut tampered = good.into_bytes();
        let i = payload_at + 5;
        tampered[i] = if tampered[i] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).unwrap();
        let (status, _, body) = get(&h, &format!("/dl/{tampered}")).await;
        assert_refused(status, &body);

        let (status, _, body) = get(&h, "/dl/not-a-token").await;
        assert_refused(status, &body);
    }

    #[tokio::test]
    async fn the_61st_download_in_an_hour_is_refused() {
        let h = ToolHarness::new(&[JANE]).await;
        mount_bytes(&h, JANE, "M1", "A1", PDF, DOWNLOADS_PER_HOUR as u64).await;
        let t = token(&h, JANE, JANE, "a.pdf");
        for _ in 0..DOWNLOADS_PER_HOUR {
            let (status, _, _) = get(&h, &format!("/dl/{t}")).await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, _, body) = get(&h, &format!("/dl/{t}")).await;
        assert_refused(status, &body);
    }

    #[test]
    fn filenames_are_reduced_to_a_safe_ascii_subset() {
        assert_eq!(safe_filename("report.pdf"), "report.pdf");
        assert_eq!(safe_filename("a\"b\r\nc;d.pdf"), "a_b__c_d.pdf");
        assert_eq!(safe_filename(""), "attachment");
        assert_eq!(safe_filename("...."), "attachment");
        assert_eq!(safe_filename(".hidden"), "hidden");
        let long = format!("{}.docx", "x".repeat(300));
        let cut = safe_filename(&long);
        assert_eq!(cut.len(), 100);
        assert!(cut.ends_with("x.docx"), "{cut}");
        let no_ext = "y".repeat(300);
        assert_eq!(safe_filename(&no_ext), "y".repeat(100));
    }
}
