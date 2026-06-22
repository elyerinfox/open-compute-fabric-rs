//! Unified error and result types shared across every fabric subsystem.

use thiserror::Error;

/// Convenience result alias used pervasively throughout the workspace.
pub type Result<T> = std::result::Result<T, Error>;

/// The canonical fabric error.
///
/// Subsystems map their failures onto these variants so that the API layer can
/// translate any error into a consistent transport representation (e.g. an HTTP
/// status code) without knowing which subsystem produced it.
#[derive(Debug, Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("already exists: {0}")]
    AlreadyExists(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("operation not supported: {0}")]
    NotSupported(String),

    #[error("unauthenticated: {0}")]
    Unauthenticated(String),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// A failure that originated inside a named pluggable provider.
    #[error("provider `{provider}` failed: {message}")]
    Provider { provider: String, message: String },

    #[error("i/o error: {0}")]
    Io(String),

    #[error("serialization error: {0}")]
    Serde(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound(what.into())
    }
    pub fn already_exists(what: impl Into<String>) -> Self {
        Self::AlreadyExists(what.into())
    }
    pub fn invalid(what: impl Into<String>) -> Self {
        Self::InvalidArgument(what.into())
    }
    pub fn unsupported(what: impl Into<String>) -> Self {
        Self::NotSupported(what.into())
    }
    pub fn forbidden(what: impl Into<String>) -> Self {
        Self::Forbidden(what.into())
    }
    pub fn unauthenticated(what: impl Into<String>) -> Self {
        Self::Unauthenticated(what.into())
    }
    pub fn internal(what: impl Into<String>) -> Self {
        Self::Internal(what.into())
    }
    pub fn provider(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Provider {
            provider: provider.into(),
            message: message.into(),
        }
    }

    /// A stable, machine-readable code for transport layers.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::AlreadyExists(_) => "already_exists",
            Error::InvalidArgument(_) => "invalid_argument",
            Error::NotSupported(_) => "not_supported",
            Error::Unauthenticated(_) => "unauthenticated",
            Error::Forbidden(_) => "forbidden",
            Error::Conflict(_) => "conflict",
            Error::Provider { .. } => "provider_error",
            Error::Io(_) => "io_error",
            Error::Serde(_) => "serde_error",
            Error::Internal(_) => "internal",
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serde(e.to_string())
    }
}
