//! Core types for pidge: accounts, configuration, and the normalized message model.
//!
//! This crate is intentionally provider-agnostic: it knows nothing about HTTP,
//! Microsoft Graph, Gmail, or authentication. It defines the [`provider::Provider`]
//! seam that `pidge-graph` and `pidge-gmail` implement.

mod account;
pub mod availability;
mod cache;
mod config;
pub mod contacts;
mod error;
mod event;
pub mod flags;
mod message;
pub mod paths;
pub mod provider;
pub mod render;
pub mod timerange;
pub mod unsubscribe;

pub use account::{Account, TokenStorage};
pub use cache::{
    CacheLookup, CachedEventRef, CachedMessageRef, EventCache, FragmentError, MessageCache,
    short_hash,
};
pub use config::{Config, Defaults, GoogleClient};
pub use contacts::{Contact, ContactSource, ContactsCache};
pub use error::CoreError;
pub use event::{
    Attendee, AttendeeKind, Calendar, Event, EventTime, RecurrenceFreq, RecurrencePattern,
    RecurrenceRange, ResponseStatus, Weekday,
};
pub use message::{Attachment, BodyContentType, FlagStatus, FullMessage, Message, MessageFrom};
