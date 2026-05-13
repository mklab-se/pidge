//! Core types for pidge: accounts, configuration, and the normalized message model.
//!
//! This crate is intentionally provider-agnostic — it knows nothing about HTTP,
//! Microsoft Graph, or authentication. Those concerns live in `pidge-client`.

mod account;
mod error;
mod message;

pub use account::Account;
pub use error::CoreError;
pub use message::{Message, MessageFrom};
