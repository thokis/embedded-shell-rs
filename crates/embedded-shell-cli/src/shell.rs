//! Shared helpers — opening a shell with the CLI's options. Supports
//! two backends:
//!
//! - When `port` is `Some`, opens a [`LinuxSerialShell`] against that
//!   serial device.
//! - When `port` is `None`, returns a [`SubprocessShell`] driving the
//!   *local* host. Useful for trying eshell against your dev machine
//!   without hooking up a device.
//!
//! The boxed trait object means every safe-to-use-locally command
//! takes the same type regardless of backend.

use std::time::Duration;

use anyhow::{Context, Result};
use embedded_shell::shell::{LinuxSerialShell, LinuxShell, Shell, SubprocessShell};

/// Open a shell — either a serial-attached Linux device or, when
/// `port` is `None`, the local host via [`SubprocessShell`].
///
/// `password` is only consulted in the serial path; the subprocess
/// path inherits the calling user's identity and `--password` is a
/// no-op there.
pub async fn open_linux(port: Option<&str>, password: Option<&str>) -> Result<Box<dyn LinuxShell>> {
    match port {
        Some(p) => {
            let mut builder = LinuxSerialShell::builder(p).login_timeout(Duration::from_secs(30));
            if let Some(pw) = password {
                builder = builder.password(pw);
            }
            let mut shell = builder
                .open()
                .await
                .with_context(|| format!("opening Linux shell on {p}"))?;
            shell
                .activate()
                .await
                .with_context(|| format!("activating Linux shell on {p}"))?;
            Ok(Box::new(shell))
        }
        None => {
            // SubprocessShell::activate is a no-op; SubprocessShell::new
            // is infallible.
            Ok(Box::new(SubprocessShell::new()))
        }
    }
}

/// Open a serial-attached Linux device specifically. Used by the
/// commands whose blast radius is too high to allow running against
/// the local host (`push`, `pull`, `reboot`).
///
/// Returns the concrete [`LinuxSerialShell`] type so callers can use
/// `LinuxSerialShell`-specific methods (notably `reboot()`).
pub async fn open_serial(port: &str, password: Option<&str>) -> Result<LinuxSerialShell> {
    let mut builder = LinuxSerialShell::builder(port).login_timeout(Duration::from_secs(30));
    if let Some(pw) = password {
        builder = builder.password(pw);
    }
    let mut shell = builder
        .open()
        .await
        .with_context(|| format!("opening Linux shell on {port}"))?;
    shell
        .activate()
        .await
        .with_context(|| format!("activating Linux shell on {port}"))?;
    Ok(shell)
}
