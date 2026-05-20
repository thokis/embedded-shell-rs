//! Shared helpers — opening a Linux shell with the CLI's options.

use std::time::Duration;

use anyhow::{Context, Result};
use embedded_shell::shell::{LinuxSerialShell, Shell};

/// Open a [`LinuxSerialShell`] on `port` and complete activation.
///
/// `password` is applied to the builder if `Some`. The login timeout
/// is fixed at 30 seconds — enough for slow embedded boots, short
/// enough that hung devices fail fast.
pub async fn open_linux(port: &str, password: Option<&str>) -> Result<LinuxSerialShell> {
    let mut builder = LinuxSerialShell::builder(port).login_timeout(Duration::from_secs(30));
    if let Some(p) = password {
        builder = builder.password(p);
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
