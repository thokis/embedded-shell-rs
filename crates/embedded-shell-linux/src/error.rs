use embedded_shell::shell::ShellError;

/// Errors that wrappers in this crate can return.
///
/// Wraps [`ShellError`] for failures that bubbled up from the
/// underlying shell, and adds a [`Parse`][Self::Parse] variant for the
/// cases where a wrapper successfully ran its device-side command but
/// couldn't make sense of the output (extremely rare; would indicate a
/// non-standard tool implementation).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The underlying [`Shell`][embedded_shell::shell::Shell] returned
    /// an error — command failure, transport error, etc.
    #[error(transparent)]
    Shell(#[from] ShellError),

    /// The wrapper ran its command successfully but couldn't parse the
    /// output. Carries a human-readable explanation; the raw stdout is
    /// available via the originating
    /// [`ShellResult`][embedded_shell::shell::ShellResult] if the caller
    /// needs it.
    #[error("parse error: {0}")]
    Parse(String),
}

/// Convenience alias used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;
