//! Error type for `pidge-core`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("config I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("config parse: {0}")]
    Parse(#[from] serde_yaml::Error),

    #[error("unknown account: {email}")]
    UnknownAccount { email: String },

    #[error("config directory unavailable on this platform")]
    NoConfigDir,

    #[error("unknown config key '{key}'")]
    UnknownConfigKey { key: String },

    #[error("invalid value '{value}' for config key '{key}'")]
    InvalidConfigValue { key: String, value: String },
}
