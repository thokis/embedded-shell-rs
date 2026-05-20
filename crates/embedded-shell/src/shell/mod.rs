//! Shell abstractions and concrete implementations.
//!
//! The [`Shell`] trait describes any async shell that can run a
//! [`Command`] and produce a [`ShellResult`]. Three implementations
//! ship in this crate:
//!
//! - [`SubprocessShell`] — runs commands locally via `sh -c`. Useful for
//!   tests, host-side scripting, and as a reference implementation.
//! - [`LinuxSerialShell`] — drives a Linux login + bash/busybox shell
//!   over a serial line, with login state machine and `\x1f`-framed
//!   exec protocol.
//! - [`UBootSerialShell`] — drives a U-Boot prompt over a serial line
//!   with `RETURNCODE=$?` framing.
//!
//! Builders ([`LinuxSerialShellBuilder`], [`UBootSerialShellBuilder`])
//! are the only public construction paths for the serial shells; they
//! validate configuration at `.open()` time and surface errors as
//! [`ShellError`].
//!
//! Errors are typed via [`ShellError`]; the [`prompts`] sub-module
//! exposes the prompt-detection helpers used by the activate state
//! machines, primarily for advanced use cases and tests.

mod command;
mod error;
mod linux;
pub mod prompts;
mod result;
mod serial;
mod subprocess;
mod traits;
mod uboot;

pub use command::{Command, DEFAULT_EXEC_TIMEOUT};
pub use error::ShellError;
pub use linux::{LinuxSerialShell, LinuxSerialShellBuilder};
pub use result::ShellResult;
pub use subprocess::SubprocessShell;
pub use traits::{LinuxShell, Shell};
pub use uboot::{UBootSerialShell, UBootSerialShellBuilder};
