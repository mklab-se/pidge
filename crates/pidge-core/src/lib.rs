//! Core types for pidge: accounts, configuration, and the normalized message model.
//!
//! This crate is intentionally provider-agnostic — it knows nothing about HTTP,
//! Microsoft Graph, or authentication. Those concerns live in `pidge-client`.

mod account;
mod cache;
mod config;
mod contacts;
mod error;
mod event;
mod message;

pub use account::{Account, TokenStorage};
pub use cache::{
    CacheLookup, CachedEventRef, CachedMessageRef, EventCache, FragmentError, MessageCache,
    short_hash,
};
pub use config::{Config, Defaults};
pub use contacts::{Contact, ContactSource, ContactsCache};
pub use error::CoreError;
pub use event::{
    Attendee, AttendeeKind, Calendar, Event, EventTime, RecurrenceFreq, RecurrencePattern,
    RecurrenceRange, ResponseStatus, Weekday,
};
pub use message::{Attachment, BodyContentType, FlagStatus, FullMessage, Message, MessageFrom};
