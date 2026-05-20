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

use std::sync::OnceLock;
use std::time::Duration;

use embedded_shell::shell::{Command, LinuxShell};
use regex::Regex;
use serde::Deserialize;

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

/// Compact summary of one unit, as returned by [`list_units`].
///
/// Mirrors the five-column layout `systemctl list-units` prints —
/// far less detail than the full [`UnitStatus`] returned by
/// [`status`], but cheap to fetch in bulk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitListEntry {
    /// Unit name (`sshd.service`, `network.target`, …).
    pub unit: String,
    /// Load state: `loaded`, `not-found`, `masked`, `error`.
    pub load: String,
    /// `active`, `inactive`, `failed`, `activating`, …
    pub active: String,
    /// More specific sub-state: `running`, `dead`, `exited`, …
    pub sub: String,
    /// One-line description.
    pub description: String,
}

impl UnitListEntry {
    /// `true` when [`active`][Self::active] is `"active"`.
    pub fn is_active(&self) -> bool {
        self.active == "active"
    }
    /// `true` when the unit is active *and* its sub-state is
    /// `"running"`.
    pub fn is_running(&self) -> bool {
        self.is_active() && self.sub == "running"
    }
}

/// Lists units systemd currently knows about.
///
/// Equivalent to `systemctl list-units -o json --no-pager`. Pass a
/// glob `pattern` to restrict the listing (`Some("sshd*")`,
/// `Some("*.service")`); pass `None` for everything systemd currently
/// has loaded.
///
/// By default `systemctl list-units` only shows units that are
/// active, have pending jobs, or have failed — the same filter
/// applies here. To see every loaded unit, use
/// `systemctl list-units --all` via a raw `shell.run` call (v1
/// doesn't expose `--all` directly).
///
/// # Errors
///
/// - [`Error::Shell`] if `systemctl` isn't installed.
/// - [`Error::Parse`] if the JSON output isn't the expected shape.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::systemd;
///
/// let services: Vec<_> = systemd::list_units(&mut shell, Some("*.service"))
///     .await?
///     .into_iter()
///     .filter(|u| !u.is_active())
///     .collect();
/// for u in services {
///     println!("{}: {} ({})", u.unit, u.active, u.sub);
/// }
/// ```
pub async fn list_units(
    shell: &mut dyn LinuxShell,
    pattern: Option<&str>,
) -> Result<Vec<UnitListEntry>> {
    let mut args = vec!["list-units", "-o", "json", "--no-pager"];
    if let Some(p) = pattern {
        args.push(p);
    }
    let result = shell.run(&systemctl(&args)).await?;
    let stdout = result.stdout().unwrap_or("");
    parse_list_units_output(stdout)
}

fn parse_list_units_output(stdout: &str) -> Result<Vec<UnitListEntry>> {
    // systemctl emits non-standard `\xNN` escapes inside JSON string
    // values for characters that are special in systemd unit naming
    // (most commonly `-` → `\x2d` in device-class unit names). Per
    // RFC 8259 the only valid arbitrary-char escape in JSON is
    // `\uXXXX`. Convert before handing to serde_json.
    let sanitized = sanitize_systemd_json(stdout);
    let raw: Vec<RawUnitListEntry> = serde_json::from_str(&sanitized)
        .map_err(|e| Error::Parse(format!("systemctl list-units json: {e}; got {stdout:?}")))?;
    Ok(raw
        .into_iter()
        .map(|r| UnitListEntry {
            unit: r.unit,
            load: r.load,
            active: r.active,
            sub: r.sub,
            description: r.description,
        })
        .collect())
}

/// Rewrites systemd-style `\xNN` escapes inside a JSON document to
/// the RFC-8259-compliant `\u00NN` form.
///
/// systemd uses `\xNN` (two hex digits) to encode characters that
/// are special in its unit-name convention — the path separator
/// `-` becomes `\x2d` in unit names like
/// `system-serial\x2dgetty.slice`. Standard JSON only accepts
/// `\uXXXX`, so serde_json rejects the raw output. This regex
/// pass is the minimum mechanical transformation that makes the
/// output parseable.
///
/// Safe because:
/// 1. systemd unit names never contain a literal backslash.
/// 2. Descriptions could in principle contain `\x` substrings, but
///    in practice they don't (systemd descriptions are human prose).
/// 3. If a `\xNN` *did* legitimately appear, the substitution
///    `\u00NN` produces the same character anyway.
fn sanitize_systemd_json(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"\\x([0-9a-fA-F]{2})").expect("systemd-xhex regex is valid"));
    re.replace_all(s, "\\u00$1").into_owned()
}

#[derive(Deserialize)]
struct RawUnitListEntry {
    unit: String,
    load: String,
    active: String,
    sub: String,
    description: String,
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

    const LIST_UNITS_JSON: &str = r#"[
        {"unit":"sshd.service","load":"loaded","active":"active","sub":"running","description":"OpenSSH server daemon","job":"-"},
        {"unit":"foo.service","load":"loaded","active":"failed","sub":"failed","description":"A broken thing"},
        {"unit":"basic.target","load":"loaded","active":"active","sub":"active","description":"Basic System"}
    ]"#;

    #[test]
    fn parses_list_units_output() {
        let entries = parse_list_units_output(LIST_UNITS_JSON).unwrap();
        assert_eq!(entries.len(), 3);

        assert_eq!(entries[0].unit, "sshd.service");
        assert!(entries[0].is_running());
        assert!(entries[0].is_active());

        assert_eq!(entries[1].active, "failed");
        assert!(!entries[1].is_active());
        assert!(!entries[1].is_running());

        // A target: active but not "running" (its sub is "active").
        assert!(entries[2].is_active());
        assert!(!entries[2].is_running());
    }

    #[test]
    fn parses_empty_list_units_output() {
        let entries = parse_list_units_output("[]").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn rejects_malformed_list_units_output() {
        let err = parse_list_units_output("not json").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    #[test]
    fn parses_systemd_xhex_escapes_in_unit_names() {
        // systemctl emits `\x2d` (i.e. `-`) inside device-class unit
        // names like the slice for serial-getty or USB device paths.
        // Standard serde_json rejects this; the sanitiser converts
        // `\xNN` → `\u00NN` before the parse.
        let input = r#"[
            {"unit":"system-serial\x2dgetty.slice","load":"loaded","active":"active","sub":"active","description":"Slice /system/serial-getty"},
            {"unit":"sys-devices-virtual-block-dm\x2d0.device","load":"loaded","active":"active","sub":"plugged","description":"/sys/devices/virtual/block/dm-0"}
        ]"#;
        let entries = parse_list_units_output(input).unwrap();
        assert_eq!(entries.len(), 2);
        // `\x2d` decoded to literal `-`.
        assert_eq!(entries[0].unit, "system-serial-getty.slice");
        assert_eq!(entries[1].unit, "sys-devices-virtual-block-dm-0.device");
    }

    #[test]
    fn sanitizer_handles_multiple_xhex_in_one_value() {
        let input = r#"[
            {"unit":"a\x2db\x2dc.device","load":"loaded","active":"active","sub":"plugged","description":"x"}
        ]"#;
        let entries = parse_list_units_output(input).unwrap();
        assert_eq!(entries[0].unit, "a-b-c.device");
    }

    #[tokio::test]
    async fn list_units_against_host_systemd() {
        if !host_has_systemctl() || !host_running_systemd() {
            eprintln!("skipping: host doesn't have a running systemd");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        // Restrict to .service units so we don't enumerate hundreds.
        let units = list_units(&mut shell, Some("*.service")).await.unwrap();
        eprintln!("[test] {} services on host", units.len());
        // A systemd host running tests should have at least one active
        // service (the test runner's parent shell, init.scope, etc.).
        assert!(units.iter().any(|u| u.is_active()));
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
