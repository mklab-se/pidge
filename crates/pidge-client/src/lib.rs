//! Microsoft 365 client and OAuth flows for the pidge CLI.
//!
//! Provides `AuthClient` (sign-in, refresh, token retrieval) and `GraphClient`
//! (Microsoft Graph API access). Depends on `pidge-core` for types.

pub mod auth;
mod error;
pub mod graph;
pub mod unsubscribe;

pub use auth::AuthClient;
pub use error::ClientError;
pub use graph::{GraphClient, Outgoing};
pub use unsubscribe::{UnsubscribeMethod, parse_unsubscribe};
