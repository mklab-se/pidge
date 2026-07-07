//! Microsoft Graph API client.

mod calendars;
pub mod events;
mod mail;
mod me;

pub use calendars::list_calendars;
pub use events::{
    EventsPage, NewEvent, RsvpKind, cancel_event, create_event, delete_event, get_event,
    list_calendar_view, list_events_at, move_event_to_calendar, move_time, rsvp_event,
    update_event,
};
pub use mail::{
    InboxPage, MailFolder, Outgoing, add_attachment, create_child_folder, create_draft,
    create_forward_draft, create_mail_folder, create_reply_all_draft, create_reply_draft,
    delete_attachment, delete_mail_folder, delete_message, fetch_message_headers, forward_message,
    get_attachment_bytes, get_categories, get_message, list_attachments, list_child_folders,
    list_drafts, list_folder_messages, list_inbox, list_mail_folders, list_messages_at, mark_read,
    mark_unread, move_message, reply_all_message, reply_message, search_messages, send_draft,
    send_mail, set_categories, set_flag, update_draft,
};
pub use me::{Me, get_me};

use crate::auth::AuthClient;
use crate::auth::config;
use crate::error::ClientError;

/// Maximum attempts for a single Graph request (1 initial + 3 retries).
const MAX_ATTEMPTS: u32 = 4;

/// Statuses worth retrying: throttling and transient gateway failures.
fn is_transient(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 503 | 504)
}

/// Send a Graph request with retry/backoff.
///
/// Honors `Retry-After` (seconds) on 429/503/504; otherwise backs off
/// exponentially (1s·2^attempt) with 0–250 ms jitter. After the attempts are
/// exhausted a throttling status becomes [`ClientError::Throttled`] so
/// callers (and agents, via exit code 5) can distinguish it. All other
/// responses — success or error — are returned for the caller to interpret.
///
/// The request must be clonable (all pidge requests carry buffered JSON/text
/// bodies); a non-clonable request is sent once without retry.
pub(crate) async fn send_with_retry(
    req: reqwest::RequestBuilder,
) -> Result<reqwest::Response, ClientError> {
    let mut attempt: u32 = 0;
    loop {
        let this_try = match req.try_clone() {
            Some(clone) => clone,
            None => return Ok(req.send().await?),
        };
        let resp = this_try.send().await?;
        let status = resp.status();
        if !is_transient(status) {
            return Ok(resp);
        }
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        attempt += 1;
        if attempt >= MAX_ATTEMPTS {
            return Err(ClientError::Throttled { retry_after });
        }
        let backoff = retry_after
            .map(std::time::Duration::from_secs)
            .unwrap_or_else(|| {
                let jitter = std::time::Duration::from_millis(u64::from(attempt) * 83 % 250);
                std::time::Duration::from_secs(1u64 << attempt.min(4)) / 2 + jitter
            });
        tracing::debug!(
            status = status.as_u16(),
            attempt,
            ?backoff,
            "retrying Graph request"
        );
        tokio::time::sleep(backoff).await;
    }
}

/// Stateful Microsoft Graph client. Holds an AuthClient and a shared HTTP client.
pub struct GraphClient {
    auth: AuthClient,
    http: reqwest::Client,
    base_url: String,
}

impl GraphClient {
    pub fn new(auth: AuthClient) -> Result<Self, ClientError> {
        Ok(Self {
            auth,
            http: reqwest::Client::builder()
                .user_agent(format!("pidge/{}", env!("CARGO_PKG_VERSION")))
                .build()?,
            base_url: config::GRAPH_BASE.to_string(),
        })
    }

    pub fn for_test(auth: AuthClient, base_url: impl Into<String>) -> Self {
        Self {
            auth,
            http: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }

    pub fn auth(&self) -> &AuthClient {
        &self.auth
    }

    /// GET /me. Used right after sign-in to learn the user's email.
    pub async fn me(&self, access_token: &str) -> Result<Me, ClientError> {
        get_me(&self.http, &self.base_url, access_token).await
    }

    /// GET /me/mailFolders/inbox/messages for a given account email.
    /// Acquires/refreshes a token transparently via `AuthClient::get_valid_token`.
    pub async fn list_inbox(
        &self,
        account: &str,
        limit: usize,
        skip: usize,
        unread_only: bool,
    ) -> Result<InboxPage, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        list_inbox(
            &self.http,
            &self.base_url,
            &token,
            account,
            limit,
            skip,
            unread_only,
        )
        .await
    }

    /// GET /me/mailFolders/{folder_id}/messages — list a custom folder.
    pub async fn list_folder(
        &self,
        account: &str,
        folder_id: &str,
        limit: usize,
        skip: usize,
        unread_only: bool,
    ) -> Result<InboxPage, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        list_folder_messages(
            &self.http,
            &self.base_url,
            &token,
            account,
            folder_id,
            limit,
            skip,
            unread_only,
        )
        .await
    }

    /// GET /me/messages with `$search="<query>"` for a given account.
    /// Fetch a page of messages at an absolute Graph continuation URL.
    pub async fn list_messages_at(
        &self,
        account: &str,
        url: &str,
    ) -> Result<InboxPage, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        list_messages_at(&self.http, &token, account, url).await
    }

    pub async fn search_messages(
        &self,
        account: &str,
        query: &str,
        limit: usize,
    ) -> Result<InboxPage, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        search_messages(&self.http, &self.base_url, &token, account, query, limit).await
    }

    /// PATCH /me/messages/{id} with `{ "isRead": false }`.
    pub async fn mark_unread(&self, account: &str, message_id: &str) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::mark_unread(&self.http, &self.base_url, &token, message_id).await
    }

    /// PATCH /me/messages/{id} with `{ "flag": { "flagStatus": "flagged"|"notFlagged" } }`.
    pub async fn set_flag(
        &self,
        account: &str,
        message_id: &str,
        flagged: bool,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::set_flag(&self.http, &self.base_url, &token, message_id, flagged).await
    }

    /// GET /me/messages/{id}?$select=categories.
    pub async fn get_categories(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<Vec<String>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::get_categories(&self.http, &self.base_url, &token, message_id).await
    }

    /// PATCH /me/messages/{id} categories.
    pub async fn set_categories(
        &self,
        account: &str,
        message_id: &str,
        categories: &[String],
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::set_categories(&self.http, &self.base_url, &token, message_id, categories).await
    }

    /// POST /me/messages/{id}/move — move to a folder by ID or well-known name.
    pub async fn move_message(
        &self,
        account: &str,
        message_id: &str,
        destination: &str,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::move_message(&self.http, &self.base_url, &token, message_id, destination).await
    }

    /// GET /me/mailFolders — list the account's top-level folders.
    pub async fn list_mail_folders(&self, account: &str) -> Result<Vec<MailFolder>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::list_mail_folders(&self.http, &self.base_url, &token).await
    }

    /// POST /me/mailFolders — create a top-level folder, returning it.
    pub async fn create_mail_folder(
        &self,
        account: &str,
        display_name: &str,
    ) -> Result<MailFolder, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::create_mail_folder(&self.http, &self.base_url, &token, display_name).await
    }

    /// GET /me/mailFolders/{parent_id}/childFolders — list a folder's children.
    pub async fn list_child_folders(
        &self,
        account: &str,
        parent_id: &str,
    ) -> Result<Vec<MailFolder>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::list_child_folders(&self.http, &self.base_url, &token, parent_id).await
    }

    /// POST /me/mailFolders/{parent_id}/childFolders — create a child folder.
    pub async fn create_child_folder(
        &self,
        account: &str,
        parent_id: &str,
        display_name: &str,
    ) -> Result<MailFolder, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::create_child_folder(&self.http, &self.base_url, &token, parent_id, display_name).await
    }

    /// DELETE /me/mailFolders/{id} — delete a folder (contents move to
    /// Deleted Items).
    pub async fn delete_mail_folder(
        &self,
        account: &str,
        folder_id: &str,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::delete_mail_folder(&self.http, &self.base_url, &token, folder_id).await
    }

    /// POST /me/sendMail — compose-and-send a new message.
    pub async fn send_mail(&self, account: &str, message: &Outgoing) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::send_mail(&self.http, &self.base_url, &token, message).await
    }

    /// POST /me/messages/{id}/reply.
    pub async fn reply_message(
        &self,
        account: &str,
        message_id: &str,
        comment: &str,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::reply_message(&self.http, &self.base_url, &token, message_id, comment).await
    }

    /// POST /me/messages/{id}/replyAll.
    pub async fn reply_all_message(
        &self,
        account: &str,
        message_id: &str,
        comment: &str,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::reply_all_message(&self.http, &self.base_url, &token, message_id, comment).await
    }

    /// POST /me/messages/{id}/forward.
    pub async fn forward_message(
        &self,
        account: &str,
        message_id: &str,
        to: &[String],
        comment: &str,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::forward_message(&self.http, &self.base_url, &token, message_id, to, comment).await
    }

    /// GET /me/mailFolders/drafts/messages.
    pub async fn list_drafts(
        &self,
        account: &str,
        limit: usize,
        skip: usize,
    ) -> Result<InboxPage, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::list_drafts(&self.http, &self.base_url, &token, account, limit, skip).await
    }

    /// POST /me/messages — create a draft, returning its new message ID.
    pub async fn create_draft(
        &self,
        account: &str,
        message: &Outgoing,
    ) -> Result<String, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::create_draft(&self.http, &self.base_url, &token, message).await
    }

    /// POST /me/messages/{id}/createReply.
    pub async fn create_reply_draft(
        &self,
        account: &str,
        message_id: &str,
        comment: &str,
    ) -> Result<String, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::create_reply_draft(&self.http, &self.base_url, &token, message_id, comment).await
    }

    /// POST /me/messages/{id}/createReplyAll.
    pub async fn create_reply_all_draft(
        &self,
        account: &str,
        message_id: &str,
        comment: &str,
    ) -> Result<String, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::create_reply_all_draft(&self.http, &self.base_url, &token, message_id, comment).await
    }

    /// POST /me/messages/{id}/createForward.
    pub async fn create_forward_draft(
        &self,
        account: &str,
        message_id: &str,
        to: &[String],
        comment: &str,
    ) -> Result<String, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::create_forward_draft(&self.http, &self.base_url, &token, message_id, to, comment)
            .await
    }

    /// POST /me/messages/{id}/send — send an existing draft.
    pub async fn send_draft(&self, account: &str, message_id: &str) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::send_draft(&self.http, &self.base_url, &token, message_id).await
    }

    /// PATCH /me/messages/{id} — overwrite a draft's editable fields.
    pub async fn update_draft(
        &self,
        account: &str,
        message_id: &str,
        message: &Outgoing,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::update_draft(&self.http, &self.base_url, &token, message_id, message).await
    }

    /// DELETE /me/messages/{id} — moves to Deleted Items. Works for both
    /// drafts and inbox messages.
    pub async fn delete_message(&self, account: &str, message_id: &str) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::delete_message(&self.http, &self.base_url, &token, message_id).await
    }

    /// POST /me/messages/{id}/attachments — attach a file (simple upload).
    pub async fn add_attachment(
        &self,
        account: &str,
        message_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<String, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::add_attachment(
            &self.http,
            &self.base_url,
            &token,
            message_id,
            name,
            content_type,
            bytes,
        )
        .await
    }

    /// DELETE /me/messages/{id}/attachments/{att_id}.
    pub async fn delete_attachment(
        &self,
        account: &str,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::delete_attachment(
            &self.http,
            &self.base_url,
            &token,
            message_id,
            attachment_id,
        )
        .await
    }

    /// GET /me/messages/{id} for a given account email.
    pub async fn get_message(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<pidge_core::FullMessage, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::get_message(&self.http, &self.base_url, &token, account, message_id).await
    }

    /// GET /me/messages/{id}?$select=internetMessageHeaders.
    pub async fn fetch_message_headers(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<Vec<(String, String)>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::fetch_message_headers(&self.http, &self.base_url, &token, message_id).await
    }

    /// GET /me/messages/{id}/attachments.
    pub async fn list_attachments(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<Vec<pidge_core::Attachment>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::list_attachments(&self.http, &self.base_url, &token, message_id).await
    }

    /// GET /me/messages/{id}/attachments/{att_id} returning decoded bytes.
    pub async fn get_attachment_bytes(
        &self,
        account: &str,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::get_attachment_bytes(
            &self.http,
            &self.base_url,
            &token,
            message_id,
            attachment_id,
        )
        .await
    }

    /// PATCH /me/messages/{id} with isRead: true.
    pub async fn mark_read(&self, account: &str, message_id: &str) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::mark_read(&self.http, &self.base_url, &token, message_id).await
    }

    // -------- Calendar surface --------

    /// GET /me/calendars.
    pub async fn list_calendars(
        &self,
        account: &str,
    ) -> Result<Vec<pidge_core::Calendar>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        calendars::list_calendars(&self.http, &self.base_url, &token, account).await
    }

    /// GET /me/calendarView (or /me/calendars/{id}/calendarView).
    pub async fn list_calendar_view(
        &self,
        account: &str,
        calendar_id: Option<&str>,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<events::EventsPage, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        events::list_calendar_view(
            &self.http,
            &self.base_url,
            &token,
            account,
            calendar_id,
            start,
            end,
            limit,
        )
        .await
    }

    /// Fetch a page of calendar events at an absolute Graph continuation URL.
    pub async fn list_events_at(
        &self,
        account: &str,
        url: &str,
    ) -> Result<EventsPage, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        list_events_at(&self.http, &token, account, url).await
    }

    /// GET /me/events/{id}.
    pub async fn get_event(
        &self,
        account: &str,
        event_id: &str,
    ) -> Result<pidge_core::Event, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        events::get_event(&self.http, &self.base_url, &token, account, event_id).await
    }

    /// POST /me/calendar/events (or /me/calendars/{id}/events).
    pub async fn create_event(
        &self,
        account: &str,
        calendar_id: Option<&str>,
        new_event: &events::NewEvent,
    ) -> Result<String, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        events::create_event(&self.http, &self.base_url, &token, calendar_id, new_event).await
    }

    /// PATCH /me/events/{id} — overwrite editable fields.
    pub async fn update_event(
        &self,
        account: &str,
        event_id: &str,
        new_event: &events::NewEvent,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        events::update_event(&self.http, &self.base_url, &token, event_id, new_event).await
    }

    /// PATCH /me/events/{id} — change only start + end.
    pub async fn move_time(
        &self,
        account: &str,
        event_id: &str,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
        tz: &str,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        events::move_time(&self.http, &self.base_url, &token, event_id, start, end, tz).await
    }

    /// DELETE /me/events/{id} — silent removal.
    pub async fn delete_event(&self, account: &str, event_id: &str) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        events::delete_event(&self.http, &self.base_url, &token, event_id).await
    }

    /// POST /me/events/{id}/cancel — organizer-only.
    pub async fn cancel_event(
        &self,
        account: &str,
        event_id: &str,
        comment: &str,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        events::cancel_event(&self.http, &self.base_url, &token, event_id, comment).await
    }

    /// POST /me/events/{id}/accept | /tentativelyAccept | /decline.
    pub async fn rsvp_event(
        &self,
        account: &str,
        event_id: &str,
        kind: events::RsvpKind,
        comment: &str,
        send_response: bool,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        events::rsvp_event(
            &self.http,
            &self.base_url,
            &token,
            event_id,
            kind,
            comment,
            send_response,
        )
        .await
    }

    /// PATCH /me/events/{id} with `calendar@odata.bind` — move between calendars.
    pub async fn move_event_to_calendar(
        &self,
        account: &str,
        event_id: &str,
        destination_calendar_id: &str,
    ) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        events::move_event_to_calendar(
            &self.http,
            &self.base_url,
            &token,
            event_id,
            destination_calendar_id,
        )
        .await
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// Responds 429 for the first N requests, then 200.
    struct FlakyResponder {
        failures: std::sync::atomic::AtomicU32,
    }

    impl Respond for FlakyResponder {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self
                .failures
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            if n > 0 {
                ResponseTemplate::new(429).insert_header("Retry-After", "0")
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true}))
            }
        }
    }

    #[tokio::test]
    async fn retries_transient_429_until_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .respond_with(FlakyResponder {
                failures: std::sync::atomic::AtomicU32::new(2),
            })
            .expect(3)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let resp = send_with_retry(http.get(format!("{}/thing", server.uri())))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn persistent_429_becomes_throttled_after_max_attempts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .expect(4)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = send_with_retry(http.get(format!("{}/thing", server.uri())))
            .await
            .unwrap_err();
        match err {
            ClientError::Throttled { retry_after } => assert_eq!(retry_after, Some(0)),
            other => panic!("expected Throttled, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_transient_errors_pass_through_without_retry() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad"))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let resp = send_with_retry(http.get(format!("{}/thing", server.uri())))
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }
}
