//! Helpers for hardware-in-the-loop tests.
//!
//! Gated behind the `test-utils` Cargo feature so production builds
//! never pull this code in. Enable it in your test crate's dev-deps:
//!
//! ```toml
//! [dev-dependencies]
//! embedded-shell = { version = "0.1", features = ["test-utils"] }
//! ```
//!
//! # What's in here
//!
//! Two layers of helpers for opening a shell against a real device
//! whose current state (Linux shell, Linux login prompt, U-Boot prompt,
//! mid-boot, mid-reboot) is unknown:
//!
//! - **`try_open_*`** — non-panicking open + activate. Returns `Err`
//!   if the device isn't in the expected state. Use these to write
//!   custom probe logic.
//! - **`open_at_*`** — defensive opens with probe-and-transition built
//!   in. `open_at_linux` falls back through U-Boot if the device isn't
//!   already at Linux; `open_at_uboot` issues a Linux reboot and
//!   catches the autoboot countdown. Both panic on unrecoverable
//!   failure (the right behavior for test setup).
//!
//! # Why these aren't in `tests/`
//!
//! The same probe logic is needed by every crate that runs hardware
//! tests against an `embedded-shell` device — `embedded-shell` itself,
//! `embedded-shell-linux`, `embedded-shell-transfer`, and any
//! downstream consumer. Hoisting them behind a feature flag avoids
//! the copy-paste alternative.

use std::time::Duration;

use tracing::debug;

use crate::shell::{LinuxSerialShell, Shell, ShellError, UBootSerialShell};

/// Settle time after closing the port. The kernel's USB-serial driver
/// holds the file descriptor briefly after the userspace fd is dropped;
/// without this delay, the next `open(2)` races and gets `EBUSY`.
pub const PORT_SETTLE: Duration = Duration::from_secs(1);

/// Try to open + activate a [`LinuxSerialShell`] on `port` within
/// `login_timeout`.
///
/// `shell_prompt` overrides the default shell-prompt regex; pass
/// `None` for the crate default.
///
/// Returns `Err` if the device isn't at a Linux shell or login prompt
/// — e.g. it's at the U-Boot prompt, rebooting, or there's no device
/// on the port.
pub async fn try_open_linux(
    port: &str,
    login_timeout: Duration,
    shell_prompt: Option<&str>,
) -> Result<LinuxSerialShell, ShellError> {
    try_open_linux_with_shutdown(port, login_timeout, Duration::from_secs(30), shell_prompt).await
}

/// Variant of [`try_open_linux`] that lets the caller pin
/// `shutdown_timeout` for the resulting shell.
///
/// Used inside [`open_at_uboot`] with a short `shutdown_timeout` so
/// the in-flight [`LinuxSerialShell::reboot_no_reactivate`] drain
/// returns quickly — we want to drop the Linux fd and grab U-Boot's
/// autoboot window before the device boots through.
pub async fn try_open_linux_with_shutdown(
    port: &str,
    login_timeout: Duration,
    shutdown_timeout: Duration,
    shell_prompt: Option<&str>,
) -> Result<LinuxSerialShell, ShellError> {
    let mut builder = LinuxSerialShell::builder(port)
        .login_timeout(login_timeout)
        .shutdown_timeout(shutdown_timeout);
    if let Some(prompt) = shell_prompt {
        builder = builder.shell_prompt(prompt);
    }
    let mut shell = builder.open().await?;
    shell.activate().await?;
    Ok(shell)
}

/// Try to open + activate a [`UBootSerialShell`] on `port` within
/// `login_timeout`.
///
/// Returns `Err` if the device isn't at U-Boot (or in its autoboot
/// countdown). When the device is in Linux, this will time out after
/// `login_timeout`.
pub async fn try_open_uboot(
    port: &str,
    login_timeout: Duration,
) -> Result<UBootSerialShell, ShellError> {
    let mut shell = UBootSerialShell::builder(port)
        .login_timeout(login_timeout)
        .open()
        .await?;
    shell.activate().await?;
    Ok(shell)
}

/// Open a Linux shell, transitioning through U-Boot if necessary.
///
/// Tries direct Linux open first (30 s budget). On failure, probes
/// U-Boot to disambiguate:
///
/// - If the device is really at U-Boot, hands off to Linux via
///   [`UBootSerialShell::boot_linux`] and retries Linux open with a
///   120 s budget for the kernel boot + login sequence.
/// - If U-Boot's probe reports a Linux login prompt (i.e. the device
///   finished autoboot just after the direct probe gave up), retries
///   Linux open with a 120 s budget.
///
/// Panics on unrecoverable failure — used in test setup, where failing
/// loudly with a diagnostic is the right behavior.
pub async fn open_at_linux(port: &str, shell_prompt: Option<&str>) -> LinuxSerialShell {
    debug!(port, "open_at_linux: trying direct Linux open (30s)");
    if let Ok(shell) = try_open_linux(port, Duration::from_secs(30), shell_prompt).await {
        debug!(port, "direct Linux open succeeded");
        return shell;
    }
    debug!(
        port,
        "direct Linux open failed; probing U-Boot to disambiguate"
    );

    tokio::time::sleep(PORT_SETTLE).await;

    match try_open_uboot(port, Duration::from_secs(30)).await {
        Ok(mut uboot) => {
            uboot.boot_linux().await.expect("boot_linux from U-Boot");
            drop(uboot);
            tokio::time::sleep(PORT_SETTLE).await;
            debug!(port, "reopening as Linux after U-Boot handoff (120s)");
            try_open_linux(port, Duration::from_secs(120), shell_prompt)
                .await
                .unwrap_or_else(|e| panic!("could not open Linux after U-Boot handoff: {e}"))
        }
        Err(ShellError::Initialization(message)) if message.contains("Linux login prompt") => {
            tokio::time::sleep(PORT_SETTLE).await;
            debug!(
                port,
                "U-Boot reports Linux login is up; retrying Linux open (120s)"
            );
            try_open_linux(port, Duration::from_secs(120), shell_prompt)
                .await
                .unwrap_or_else(|e| panic!("retry Linux open after U-Boot hint: {e}"))
        }
        Err(e) => panic!("could not reach U-Boot to transition into Linux: {e}"),
    }
}

/// Open a U-Boot shell, transitioning through a Linux reboot if
/// necessary.
///
/// Tries direct U-Boot open first (15 s budget — short, because if
/// the device is already at U-Boot the prompt comes back fast). On
/// failure, opens Linux (90 s budget), sends `reboot` via
/// [`LinuxSerialShell::reboot_no_reactivate`] with a tight 2 s
/// `shutdown_timeout` so the drain returns quickly, then re-opens
/// as U-Boot with a 90 s budget to catch the autoboot countdown.
///
/// Panics on unrecoverable failure.
pub async fn open_at_uboot(port: &str, shell_prompt: Option<&str>) -> UBootSerialShell {
    debug!(port, "open_at_uboot: trying direct U-Boot open (15s)");
    if let Ok(shell) = try_open_uboot(port, Duration::from_secs(15)).await {
        debug!(port, "direct U-Boot open succeeded");
        return shell;
    }
    debug!(
        port,
        "direct U-Boot open failed; assuming device is in Linux; transitioning"
    );

    tokio::time::sleep(PORT_SETTLE).await;

    {
        let mut linux = try_open_linux_with_shutdown(
            port,
            Duration::from_secs(90),
            Duration::from_secs(2),
            shell_prompt,
        )
        .await
        .unwrap_or_else(|e| {
            panic!("could not reach Linux to issue reboot for U-Boot transition: {e}")
        });
        debug!(
            port,
            "issuing reboot from Linux (no reactivate, short drain)"
        );
        linux
            .reboot_no_reactivate()
            .await
            .expect("reboot_no_reactivate");
        // Drop releases the port.
    }
    tokio::time::sleep(PORT_SETTLE).await;

    debug!(
        port,
        "reopening as U-Boot to catch autoboot countdown (90s)"
    );
    try_open_uboot(port, Duration::from_secs(90))
        .await
        .unwrap_or_else(|e| panic!("could not open U-Boot after Linux reboot: {e}"))
}
