use std::fmt;

/// Classification of an error, used to pick a user-facing message and to
/// decide whether a retry is worth attempting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Local file or stream I/O failed.
    Io,
    /// A remote fetch failed. Switching mirrors or retrying may help.
    Network,
    /// The input file could not be decoded to PCM.
    Decode,
    /// A model is missing, corrupt, or unusable.
    Model,
    /// The recognition engine failed.
    Engine,
    /// An output sink failed.
    Sink,
    /// Component lookup or construction failed.
    Registry,
    /// The job was cancelled by the user.
    Cancelled,
    /// A bug. Should not be reachable through normal use.
    Internal,
}

/// The single error type used across the core crate.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Io, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "cancelled")
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}

/// A cloneable, source-free snapshot of an [`Error`], suitable for carrying
/// across the event bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorInfo {
    pub kind: ErrorKind,
    pub message: String,
}

impl From<&Error> for ErrorInfo {
    fn from(err: &Error) -> Self {
        Self {
            kind: err.kind,
            message: err.message.clone(),
        }
    }
}

impl fmt::Display for ErrorInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
