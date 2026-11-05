//! Operation errors: one taxonomy for every frontend. The CLI turns the kind
//! into an exit code, the MCP servers into a one-line tool error.

use pidge_core::provider::ProviderError;
use pidge_store::StoreError;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Bad input: fix the call.
    Usage,
    NotFound,
    /// A name or fragment matched several things; the message lists them.
    Ambiguous,
    /// A mailbox must sign in again.
    AuthExpired,
    Throttled,
    /// The user's policy forbids this.
    GuardrailDenied,
    /// The provider or account type cannot do this.
    Unsupported,
    /// Anything else from a provider.
    Provider,
    Internal,
}

impl ErrorKind {
    /// The CLI's exit code (documented in CLAUDE.md).
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Usage => 2,
            Self::AuthExpired => 3,
            Self::NotFound | Self::Ambiguous => 4,
            Self::Throttled => 5,
            Self::GuardrailDenied => 6,
            Self::Unsupported | Self::Provider | Self::Internal => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::AuthExpired => "auth_expired",
            Self::Throttled => "throttled",
            Self::GuardrailDenied => "guardrail_denied",
            Self::Unsupported => "unsupported",
            Self::Provider => "provider",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OpError {
    pub kind: ErrorKind,
    pub message: String,
    /// What to do about it, for the agent.
    pub hint: Option<String>,
}

impl OpError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            hint: None,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Usage, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The message with its hint, on as few lines as possible.
    pub fn full(&self) -> String {
        match &self.hint {
            Some(h) => format!("{} ({h})", self.message),
            None => self.message.clone(),
        }
    }
}

impl From<ProviderError> for OpError {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::Auth { account, .. } => OpError::new(
                ErrorKind::AuthExpired,
                format!("{account} has to sign in again"),
            )
            .with_hint("accounts action=connect with that address reconnects it"),
            ProviderError::NotFound(m) => OpError::not_found(format!("not found: {m}")),
            ProviderError::Throttled { retry_after } => OpError::new(
                ErrorKind::Throttled,
                match retry_after {
                    Some(s) => format!("the mail service is busy; try again in {s} s"),
                    None => "the mail service is busy; try again shortly".into(),
                },
            ),
            ProviderError::Unsupported(m) => OpError::new(ErrorKind::Unsupported, m),
            ProviderError::Invalid(m) => OpError::usage(m),
            ProviderError::CursorExpired => {
                OpError::new(ErrorKind::Provider, "sync state expired; it will rebuild")
            }
            ProviderError::Other(m) => OpError::new(ErrorKind::Provider, m),
        }
    }
}

impl From<StoreError> for OpError {
    fn from(e: StoreError) -> Self {
        OpError::internal(e.to_string())
    }
}

pub type OpResult<T> = Result<T, OpError>;
