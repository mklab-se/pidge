//! Microsoft Graph API client.

mod mail;
mod me;

pub use mail::{get_attachment_bytes, get_message, list_attachments, list_inbox, mark_read};
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
        unread_only: bool,
    ) -> Result<Vec<Message>, ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        list_inbox(
            &self.http,
            &self.base_url,
            &token,
            account,
            limit,
            unread_only,
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
        mail::get_attachment_bytes(&self.http, &self.base_url, &token, message_id, attachment_id).await
    }

    /// PATCH /me/messages/{id} with isRead: true.
    pub async fn mark_read(&self, account: &str, message_id: &str) -> Result<(), ClientError> {
        let token = self.auth.get_valid_token(account).await?;
        mail::mark_read(&self.http, &self.base_url, &token, message_id).await
    }
}
