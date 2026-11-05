//! Core types for pidge: accounts, configuration, and the normalized message model.
//!
//! This crate is intentionally provider-agnostic: it knows nothing about HTTP,
//! Microsoft Graph, Gmail, or authentication. It defines the [`provider::Provider`]
//! seam that `pidge-client` (Microsoft Graph) and `pidge-gmail` implement.

mod account;
pub mod availability;
mod config;
mod error;
mod event;
mod message;
pub mod paths;
pub mod provider;
pub mod render;
pub mod timerange;
pub mod unsubscribe;

pub use account::{Account, TokenStorage};
pub use config::{Config, Defaults, GoogleClient};
pub use error::CoreError;
pub use event::{
    Attendee, AttendeeKind, Calendar, Event, EventTime, RecurrenceFreq, RecurrencePattern,
    RecurrenceRange, ResponseStatus, Weekday,
};
pub use message::{Attachment, BodyContentType, FlagStatus, FullMessage, Message, MessageFrom};
