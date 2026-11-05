//! Gmail and Google Calendar behind pidge's provider seam
//! ([`pidge_core::provider::Provider`]).
//!
//! The mapping (design §13): the `INBOX` label is the inbox, archiving
//! removes it, deleting trashes; user labels are categories (and the
//! targets of folder moves); `CATEGORY_*` tabs are bulk hints; `STARRED` is
//! the flag; `history.list` is the delta; drafts are raw RFC 5322 messages;
//! the calendar is the user's `primary` Google calendar, recurring events
//! expanded, with `sendUpdates=all` on changes.
//!
//! - [`auth`]: Google sign-in (loopback + PKCE), refresh, [`GoogleAuth`].
//! - [`GmailProvider`]: the [`pidge_core::provider::Provider`] implementation.

pub mod auth;
mod calendar;
mod http;
mod mime;
mod model;
mod provider;
mod query;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use auth::{GoogleAuth, GoogleOAuthConfig};
pub use provider::{CALENDAR_BASE, GMAIL_BASE, GmailProvider};
