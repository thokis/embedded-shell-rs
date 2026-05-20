//! Async driver for Linux and U-Boot devices accessed over a serial line.
//!
//! The crate is built around a single [`Shell`] trait: any concrete shell
//! (local subprocess, Linux over serial, U-Boot over serial) exposes the
//! same `activate` / `deactivate` / `run` interface, takes a [`Command`]
//! built fluently, and produces a typed [`ShellResult`] or a
//! [`ShellError`] variant.
//!
//! On the wire, every Linux command runs under a `timeout(1)` wrapper and
//! is framed with three `\x1f` (Unit Separator) sentinels so stdout,
//! stderr, and exit code can be read back byte-exactly — no
//! prompt-matching, no ambiguity from prompt-shaped strings appearing in
//! command output. U-Boot uses a `RETURNCODE=$?` framing variant because
//! its `echo` doesn't interpret `\x1f`.
//!
//! # Getting started
//!
//! ```ignore
//! use embedded_shell::shell::{Command, LinuxSerialShell, Shell};
//!
//! let mut shell = LinuxSerialShell::builder("/dev/ttyUSB0")
//!     .password("raspberry") // omit for autologin / passwordless images
//!     .open()
//!     .await?;
//! shell.activate().await?;
//!
//! let r = shell.run(&Command::new("uname").arg("-a")).await?;
//! println!("{}", r.stdout().unwrap_or(""));
//! ```
//!
//! For the full event schema, stability promise, and recipes for
//! configuring `tracing` (including journald integration), see the
//! crate's `README.md`.
//!
//! [`Shell`]: shell::Shell
//! [`Command`]: shell::Command
//! [`ShellResult`]: shell::ShellResult
//! [`ShellError`]: shell::ShellError

pub mod shell;

/// Helpers for writing hardware-in-the-loop tests against an
/// `embedded-shell` device. Gated behind the `test-utils` feature.
#[cfg(feature = "test-utils")]
pub mod test_utils;
