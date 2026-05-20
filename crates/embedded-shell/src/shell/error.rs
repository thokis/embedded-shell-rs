use std::io;
use std::time::Duration;

use thiserror::Error;

use super::result::ShellResult;

/// Errors produced by any [`Shell`][super::Shell] operation in this crate.
///
/// Variants are designed to be pattern-matched: each represents a
/// specific failure mode, and each carries the structured context a
/// caller needs to react. There is no catch-all string variant — if a
/// new failure mode is needed, it gets its own variant in a future
/// version (a minor-version-compatible addition; renaming or removing
/// variants is a breaking change).
///
/// # Variant guide
///
/// | Variant | When |
/// |---|---|
/// | [`Initialization`][Self::Initialization] | Shell setup failed before any command ran (login timeout, invalid configuration). |
/// | [`CommandFailed`][Self::CommandFailed] | Command ran, exit code was non-zero (and [`Command::allow_nonzero`][super::Command::allow_nonzero] wasn't set). |
/// | [`CommandNotFound`][Self::CommandNotFound] | Exit code 127: the device's shell couldn't find the binary. |
/// | [`Timeout`][Self::Timeout] | Device-side `timeout(1)` killed the command (Linux exit 124). |
/// | [`ReadTimeout`][Self::ReadTimeout] | Host-side read deadline expired waiting for the framed response. |
/// | [`InvalidRegex`][Self::InvalidRegex] | A user-supplied prompt regex failed to compile. |
/// | [`Io`][Self::Io] | Transport-level I/O failure (port disconnected, etc.). |
#[derive(Debug, Error)]
pub enum ShellError {
    /// Shell setup failed before any command could run. Carries a
    /// human-readable message describing what went wrong (login
    /// timeout, missing port, configuration validation failure, …).
    #[error("shell initialization failed: {0}")]
    Initialization(String),

    /// The command ran to completion but returned a non-zero exit
    /// code, and [`Command::allow_nonzero`][super::Command::allow_nonzero]
    /// was not set. The full [`ShellResult`] (stdout, stderr, exit
    /// code, timing) is attached.
    #[error("command {:?} failed with exit-code {}", .0.command(), .0.exit_code())]
    CommandFailed(Box<ShellResult>),

    /// The device's shell exited with code 127 — typically meaning the
    /// requested binary doesn't exist. The `command` field is the
    /// first whitespace-separated token of the wire string (the
    /// "binary name"); the full result is attached for diagnostics.
    #[error("command not found: {command}")]
    CommandNotFound {
        /// First-token name of the missing binary.
        command: String,
        /// Full result including the device's error message in stdout
        /// or stderr.
        result: Box<ShellResult>,
    },

    /// Device-side `timeout(1)` killed the command (exit code 124 on
    /// Linux shells). `duration` is the configured timeout that
    /// triggered the kill.
    #[error("command {:?} timed out after {duration:?}", result.command())]
    Timeout {
        /// The timeout originally requested in
        /// [`Command::timeout`][super::Command::timeout].
        duration: Duration,
        /// Whatever partial output was captured before the kill.
        result: Box<ShellResult>,
    },

    /// The host-side read deadline expired while waiting for a framed
    /// response from the device. Distinct from [`Timeout`][Self::Timeout]
    /// (which is a device-side `timeout(1)` kill); this one means the
    /// device hasn't replied with the expected framing bytes within
    /// the timeout. Carries everything the transport had captured up
    /// to that point, for diagnostics.
    #[error(
        "transport read timed out after {duration:?} with {captured_len} byte(s) captured",
        captured_len = captured.len()
    )]
    ReadTimeout {
        /// How long the host was willing to wait.
        duration: Duration,
        /// Raw bytes the transport accumulated before giving up.
        captured: Vec<u8>,
    },

    /// A user-supplied regex pattern (prompt detector, etc.) failed to
    /// compile. Surfaced from builder `.open()` calls when a custom
    /// `shell_prompt` or `login_prompt` is invalid.
    #[error("invalid regex {pattern:?}: {source}")]
    InvalidRegex {
        /// The offending pattern, verbatim.
        pattern: String,
        /// The underlying compile error from the `regex` crate.
        #[source]
        source: regex::Error,
    },

    /// Transport-level I/O failure: port disconnect, broken pipe,
    /// permission denied, etc. Use `reconnect()` on the shell to
    /// recover from transient disconnects.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl ShellError {
    /// Construct an [`Initialization`][Self::Initialization] variant
    /// from anything convertible to `String`. Convenience for places
    /// where a `format!`-style message is the natural error shape.
    pub fn initialization(message: impl Into<String>) -> Self {
        Self::Initialization(message.into())
    }
}
