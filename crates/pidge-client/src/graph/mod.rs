//! Microsoft Graph API client.

mod mail;
mod me;

pub use mail::{
    InboxPage, Outgoing, add_attachment, create_draft, create_forward_draft,
    create_reply_all_draft, create_reply_draft, delete_attachment, delete_message, forward_message,
    get_attachment_bytes, get_message, list_attachments, list_drafts, list_inbox, mark_read,
    mark_unread, move_message, reply_all_message, reply_message, search_messages, send_draft,
    send_mail, set_flag, update_draft,
};
pub use me::{Me, get_me};

use crate::auth::AuthClient;
use crate::auth::config;
use crate::error::ClientError;
use pidge_core::Message;

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

    /// GET /me/messages with `$search="<query>"` for a given account.
    pub async fn search_messages(
        &self,
        account: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Message>, ClientError> {
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
}
