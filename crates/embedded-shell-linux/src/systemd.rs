//! Wrappers around `systemctl` for managing systemd units on the
//! device.
//!
//! Enabled by the opt-in `systemd` Cargo feature.
//!
//! # Device-side requirement
//!
//! A working systemd installation with `systemctl` on the device's
//! `PATH`. Most state-changing operations ([`start`], [`stop`],
//! [`restart`], [`reload`], [`enable`], [`disable`]) require root or
//! a polkit policy that grants them — this crate does not elevate
//! privileges, so calls will fail with
//! [`Error::Shell`] if the device-side user can't authorize them.
//!
//! # Surface
//!
//! Read-only queries (cheap, safe to call from any user):
//! [`is_active`], [`is_enabled`], [`is_failed`], [`status`].
//!
//! State-changing operations (typically need root):
//! [`start`], [`stop`], [`restart`], [`reload`], [`enable`], [`disable`].
//!
//! For richer state inspection, call [`status`] and look at the
//! returned [`UnitStatus`].
//!
//! [`LinuxShell`]: embedded_shell::shell::LinuxShell

use std::time::Duration;

use embedded_shell::shell::{Command, LinuxShell};

use crate::error::{Error, Result};

/// Default timeout for state-changing operations. systemctl waits
/// for the unit transition to complete before returning — slow
/// services (databases, big web apps) routinely take 10–20 s to
/// start. The crate-default exec timeout (~5 s) is too tight.
const MUTATING_TIMEOUT: Duration = Duration::from_secs(30);

/// Structured view of a unit's runtime state, parsed from
/// `systemctl show`.
///
/// Fields are raw strings as systemd returns them — they're stable
/// per the systemd documentation and free of escapes once they reach
/// us. The helper methods (`is_active`, `is_running`, …) cover the
/// most-asked-for boolean questions; reach into the string fields
/// directly for anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitStatus {
    /// `active`, `inactive`, `failed`, `activating`, `deactivating`,
    /// `reloading`, …
    pub active_state: String,
    /// More specific sub-state (`running`, `dead`, `exited`,
    /// `start-pre`, …). Interpretation depends on the unit type.
    pub sub_state: String,
    /// `loaded`, `not-found`, `masked`, `error`, …
    pub load_state: String,
    /// Persistent enablement state (`enabled`, `disabled`, `static`,
    /// `masked`, `generated`, …). `None` for transient units that
    /// have no on-disk unit file.
    pub unit_file_state: Option<String>,
    /// Human-readable description (`Description=` line from the unit
    /// file).
    pub description: String,
}

impl UnitStatus {
    /// `true` when [`active_state`][Self::active_state] is `"active"`.
    /// Includes services that are currently running and oneshots that
    /// completed successfully.
    pub fn is_active(&self) -> bool {
        self.active_state == "active"
    }

    /// `true` when the unit is active *and* its sub-state is
    /// `"running"`. Use this when you specifically want "is the
    /// process alive" rather than "did the unit start successfully"
    /// (which is the distinction between a long-running service and
    /// a completed oneshot).
    pub fn is_running(&self) -> bool {
        self.is_active() && self.sub_state == "running"
    }

    /// `true` when [`active_state`][Self::active_state] is `"failed"`.
    pub fn is_failed(&self) -> bool {
        self.active_state == "failed"
    }

    /// `true` when the unit has a persistent enabled state on disk.
    /// Aliases and runtime-enabled units also count.
    pub fn is_enabled(&self) -> bool {
        matches!(
            self.unit_file_state.as_deref(),
            Some("enabled") | Some("enabled-runtime") | Some("alias")
        )
    }
}

/// Check whether `unit` is currently active.
///
/// Equivalent to `systemctl is-active <unit>`. Returns `true` only if
/// the unit is in the `active` state — `activating`, `reloading`,
/// `inactive`, and `failed` all return `false`. For richer state,
/// call [`status`].
///
/// # Errors
///
/// - [`Error::Shell`] if `systemctl` isn't installed (systemd isn't
///   present on the device).
pub async fn is_active(shell: &mut dyn LinuxShell, unit: &str) -> Result<bool> {
    let result = shell
        .run(&systemctl(&["is-active", unit]).allow_nonzero())
        .await?;
    Ok(result.stdout().unwrap_or("").trim() == "active")
}

/// Check whether `unit` is enabled to start at boot.
///
/// Equivalent to `systemctl is-enabled <unit>`. `true` only for
/// states that mean "yes, this will start" — `enabled`,
/// `enabled-runtime`, and `alias`. `static`, `disabled`, `masked`,
/// `generated`, `indirect`, and unknown units all return `false`.
///
/// # Errors
///
/// - [`Error::Shell`] if `systemctl` isn't installed.
pub async fn is_enabled(shell: &mut dyn LinuxShell, unit: &str) -> Result<bool> {
    let result = shell
        .run(&systemctl(&["is-enabled", unit]).allow_nonzero())
        .await?;
    Ok(matches!(
        result.stdout().unwrap_or("").trim(),
        "enabled" | "enabled-runtime" | "alias"
    ))
}

/// Check whether `unit` is in the `failed` state.
///
/// Equivalent to `systemctl is-failed <unit>`.
///
/// # Errors
///
/// - [`Error::Shell`] if `systemctl` isn't installed.
pub async fn is_failed(shell: &mut dyn LinuxShell, unit: &str) -> Result<bool> {
    let result = shell
        .run(&systemctl(&["is-failed", unit]).allow_nonzero())
        .await?;
    Ok(result.stdout().unwrap_or("").trim() == "failed")
}

/// Start `unit`.
///
/// # Errors
///
/// - [`Error::Shell`] if the operation fails — most commonly because
///   the caller isn't root and polkit didn't authorize the action,
///   or because the unit doesn't exist.
pub async fn start(shell: &mut dyn LinuxShell, unit: &str) -> Result<()> {
    shell
        .run(&systemctl(&["start", unit]).timeout(MUTATING_TIMEOUT))
        .await?;
    Ok(())
}

/// Stop `unit`.
///
/// # Errors
///
/// As for [`start`].
pub async fn stop(shell: &mut dyn LinuxShell, unit: &str) -> Result<()> {
    shell
        .run(&systemctl(&["stop", unit]).timeout(MUTATING_TIMEOUT))
        .await?;
    Ok(())
}

/// Restart `unit` — stops and starts it again.
///
/// If the unit isn't running, this just starts it.
///
/// # Errors
///
/// As for [`start`].
pub async fn restart(shell: &mut dyn LinuxShell, unit: &str) -> Result<()> {
    shell
        .run(&systemctl(&["restart", unit]).timeout(MUTATING_TIMEOUT))
        .await?;
    Ok(())
}

/// Reload `unit` — asks the unit to re-read its configuration without
/// restarting. Only works for units whose service supports reload
/// (nginx, sshd, …). For units that don't, this is an error;
/// `restart` is the safer general-purpose op.
///
/// # Errors
///
/// As for [`start`], plus an additional failure mode when the unit
/// doesn't support reload.
pub async fn reload(shell: &mut dyn LinuxShell, unit: &str) -> Result<()> {
    shell
        .run(&systemctl(&["reload", unit]).timeout(MUTATING_TIMEOUT))
        .await?;
    Ok(())
}

/// Enable `unit` to start at boot.
///
/// Does **not** also start the unit — to start it now, call [`start`]
/// after this returns. (The combined `systemctl enable --now` is not
/// exposed; calling two functions explicitly is clearer at the call
/// site.)
///
/// # Errors
///
/// As for [`start`].
pub async fn enable(shell: &mut dyn LinuxShell, unit: &str) -> Result<()> {
    shell
        .run(&systemctl(&["enable", unit]).timeout(MUTATING_TIMEOUT))
        .await?;
    Ok(())
}

/// Disable `unit` so it doesn't start at boot.
///
/// Does **not** also stop the unit — to stop it now, call [`stop`]
/// after this returns.
///
/// # Errors
///
/// As for [`start`].
pub async fn disable(shell: &mut dyn LinuxShell, unit: &str) -> Result<()> {
    shell
        .run(&systemctl(&["disable", unit]).timeout(MUTATING_TIMEOUT))
        .await?;
    Ok(())
}

/// Fetch the current state of `unit` as a structured [`UnitStatus`].
///
/// Equivalent to `systemctl show <unit> --property=…` with the five
/// properties needed by [`UnitStatus`]. Cheap and safe to call from
/// any user.
///
/// # Errors
///
/// - [`Error::Shell`] if `systemctl` isn't installed.
/// - [`Error::Parse`] if the output doesn't contain the expected
///   `Key=Value` lines (extremely unlikely; would indicate a
///   non-standard `systemctl` implementation).
pub async fn status(shell: &mut dyn LinuxShell, unit: &str) -> Result<UnitStatus> {
    let result = shell
        .run(&systemctl(&[
            "show",
            unit,
            "--property=ActiveState,SubState,LoadState,UnitFileState,Description",
            "--no-pager",
        ]))
        .await?;
    let stdout = result
        .stdout()
        .ok_or_else(|| Error::Parse(format!("systemctl show produced no output for {unit:?}")))?;
    parse_show_output(stdout)
}

/// Builds the base `systemctl` command. `--no-pager` keeps `show` /
/// `status` from blocking on a pager that doesn't exist in
/// non-interactive sessions.
fn systemctl(args: &[&str]) -> Command {
    let mut cmd = Command::new("systemctl");
    for a in args {
        cmd = cmd.arg(*a);
    }
    cmd
}

fn parse_show_output(stdout: &str) -> Result<UnitStatus> {
    let mut active_state = None;
    let mut sub_state = None;
    let mut load_state = None;
    let mut unit_file_state = None;
    let mut description = None;

    for line in stdout.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "ActiveState" => active_state = Some(value.to_string()),
            "SubState" => sub_state = Some(value.to_string()),
            "LoadState" => load_state = Some(value.to_string()),
            "UnitFileState" => {
                // systemctl emits `UnitFileState=` with an empty value
                // for transient/generated units. Normalize to None so
                // callers can match cleanly.
                unit_file_state = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            "Description" => description = Some(value.to_string()),
            _ => {}
        }
    }

    Ok(UnitStatus {
        active_state: active_state
            .ok_or_else(|| Error::Parse("systemctl show: missing ActiveState".to_string()))?,
        sub_state: sub_state
            .ok_or_else(|| Error::Parse("systemctl show: missing SubState".to_string()))?,
        load_state: load_state
            .ok_or_else(|| Error::Parse("systemctl show: missing LoadState".to_string()))?,
        unit_file_state,
        description: description.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOW_RUNNING_SERVICE: &str = "\
ActiveState=active
SubState=running
LoadState=loaded
UnitFileState=enabled
Description=Some Daemon
";

    const SHOW_FAILED_SERVICE: &str = "\
ActiveState=failed
SubState=failed
LoadState=loaded
UnitFileState=enabled
Description=Broken Daemon
";

    const SHOW_TRANSIENT_UNIT: &str = "\
ActiveState=active
SubState=running
LoadState=loaded
UnitFileState=
Description=Transient session scope
";

    const SHOW_COMPLETED_ONESHOT: &str = "\
ActiveState=active
SubState=exited
LoadState=loaded
UnitFileState=static
Description=One-shot job that's done
";

    #[test]
    fn parses_running_service() {
        let s = parse_show_output(SHOW_RUNNING_SERVICE).unwrap();
        assert_eq!(s.active_state, "active");
        assert_eq!(s.sub_state, "running");
        assert_eq!(s.unit_file_state.as_deref(), Some("enabled"));
        assert!(s.is_active());
        assert!(s.is_running());
        assert!(s.is_enabled());
        assert!(!s.is_failed());
    }

    #[test]
    fn parses_failed_service() {
        let s = parse_show_output(SHOW_FAILED_SERVICE).unwrap();
        assert!(s.is_failed());
        assert!(!s.is_active());
        assert!(!s.is_running());
    }

    #[test]
    fn transient_unit_has_no_unit_file_state() {
        let s = parse_show_output(SHOW_TRANSIENT_UNIT).unwrap();
        assert_eq!(s.unit_file_state, None);
        assert!(!s.is_enabled());
    }

    #[test]
    fn completed_oneshot_is_active_but_not_running() {
        let s = parse_show_output(SHOW_COMPLETED_ONESHOT).unwrap();
        assert!(s.is_active());
        assert!(!s.is_running());
    }

    #[test]
    fn rejects_output_missing_required_fields() {
        let err = parse_show_output("ActiveState=active\n").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    fn host_has_systemctl() -> bool {
        std::process::Command::new("sh")
            .args(["-c", "command -v systemctl"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn host_running_systemd() -> bool {
        std::process::Command::new("sh")
            .args([
                "-c",
                "systemctl is-system-running >/dev/null 2>&1; test $? -ne 127",
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn status_via_subprocess_shell_against_journald() {
        if !host_has_systemctl() || !host_running_systemd() {
            eprintln!("skipping: host doesn't have a running systemd");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        // systemd-journald is universal on any systemd-running host.
        let s = status(&mut shell, "systemd-journald.service")
            .await
            .unwrap();
        eprintln!("[test] systemd-journald status: {s:?}");
        // We don't assert "running" — on heavily customised systems it
        // could theoretically be configured otherwise — but the load
        // state should always be "loaded" since the unit ships with
        // systemd itself.
        assert_eq!(s.load_state, "loaded");
        assert!(!s.description.is_empty());
    }

    #[tokio::test]
    async fn is_active_returns_true_for_journald() {
        if !host_has_systemctl() || !host_running_systemd() {
            eprintln!("skipping: host doesn't have a running systemd");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        let active = is_active(&mut shell, "systemd-journald.service")
            .await
            .unwrap();
        assert!(active, "journald should be active on a systemd host");
    }
}
