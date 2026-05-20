//! Wrappers around `nmcli` for reading NetworkManager state.
//!
//! Enabled by the opt-in `networkmanager` Cargo feature.
//!
//! # Device-side requirement
//!
//! An `nmcli` binary on the device's `PATH`, talking to a running
//! NetworkManager daemon. NM is common on Yocto and Debian-based
//! images but absent on most minimal busybox installs (OpenWrt uses
//! its own UCI/netifd stack instead).
//!
//! # Surface
//!
//! Read-only in v1:
//!
//! - [`connections`] — every known connection profile (active or not).
//! - [`active_connections`] — only the currently-active subset.
//! - [`devices`] — every network device NM manages, with state.
//!
//! State-changing operations (`nmcli connection up/down`, `connection
//! modify`, `device disconnect`, …) aren't exposed yet. Same
//! reasoning as the [`crate::iproute2`] module: a wrong call against
//! the network you're operating over can disconnect the device, and
//! the design of structured arguments (especially for `modify`)
//! deserves dedicated thought. Drop into `shell.run(Command::new("nmcli")
//! .args(["connection", "up", "Home WiFi"]))` directly when needed.
//!
//! # Parsing
//!
//! Output is requested via `nmcli -t -f field1,field2,... <subcommand>`
//! — the documented "terse" format: one record per line, fields
//! separated by `:`, literal colons in values escaped as `\:` and
//! literal backslashes as `\\`. The internal terse parser handles
//! both escapes correctly.
//!
//! [`LinuxShell`]: embedded_shell::shell::LinuxShell

use embedded_shell::shell::{Command, LinuxShell};

use crate::error::{Error, Result};

/// One NetworkManager connection profile.
///
/// A "connection" in NM terms is a saved configuration (Wi-Fi
/// credentials, ethernet IP settings, VPN config, …) — it may or may
/// not be currently in use. When it *is* in use, [`device`][Self::device]
/// names which device it's bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// User-visible name (e.g. `"Wired connection 1"`, `"Home WiFi"`).
    pub name: String,
    /// Stable UUID. Use this rather than `name` when scripting against
    /// connections, since names can be renamed.
    pub uuid: String,
    /// Connection type as NM reports it: `ethernet`, `wifi`, `gsm`,
    /// `bridge`, `vpn`, `loopback`, …
    pub kind: String,
    /// Device this connection is bound to right now, or `None` when
    /// the connection is idle.
    pub device: Option<String>,
}

impl Connection {
    /// `true` when the connection is currently bound to a device.
    pub fn is_active(&self) -> bool {
        self.device.is_some()
    }
}

/// One network device NM manages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Interface name (`eth0`, `wlan0`, …).
    pub name: String,
    /// Device type: `ethernet`, `wifi`, `loopback`, `gsm`, `bridge`,
    /// `tun`, …
    pub kind: String,
    /// State string as nmcli reports it: `connected`, `disconnected`,
    /// `unmanaged`, `unavailable`, `connecting`, `disconnecting`, …
    pub state: String,
    /// Active connection on this device, if any.
    pub connection: Option<String>,
}

impl Device {
    /// `true` when the device is connected, including the
    /// `"connected (externally)"` and `"connected (site)"` variants
    /// nmcli emits when NM didn't bring the device up itself.
    /// Disconnected, unavailable, and connecting states all return
    /// `false`.
    pub fn is_connected(&self) -> bool {
        self.state.starts_with("connected")
    }
}

/// Lists every NetworkManager connection profile (active or not).
///
/// # Errors
///
/// - [`Error::Shell`] if `nmcli` isn't installed or the daemon isn't
///   reachable.
/// - [`Error::Parse`] if a returned line doesn't have the four
///   expected fields.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::networkmanager;
///
/// for c in networkmanager::connections(&mut shell).await? {
///     println!(
///         "{:24} {} ({})",
///         c.name,
///         c.kind,
///         if c.is_active() { "active" } else { "idle" },
///     );
/// }
/// ```
pub async fn connections(shell: &mut dyn LinuxShell) -> Result<Vec<Connection>> {
    let stdout = run_nmcli(
        shell,
        &["-t", "-f", "NAME,UUID,TYPE,DEVICE", "connection", "show"],
    )
    .await?;
    parse_connections(&stdout)
}

/// Lists only the currently-active connections.
///
/// # Errors
///
/// As for [`connections`].
pub async fn active_connections(shell: &mut dyn LinuxShell) -> Result<Vec<Connection>> {
    let stdout = run_nmcli(
        shell,
        &[
            "-t",
            "-f",
            "NAME,UUID,TYPE,DEVICE",
            "connection",
            "show",
            "--active",
        ],
    )
    .await?;
    parse_connections(&stdout)
}

/// Lists every network device NM manages, with its current state.
///
/// # Errors
///
/// As for [`connections`].
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::networkmanager;
///
/// let online: Vec<_> = networkmanager::devices(&mut shell)
///     .await?
///     .into_iter()
///     .filter(|d| d.is_connected())
///     .collect();
/// ```
pub async fn devices(shell: &mut dyn LinuxShell) -> Result<Vec<Device>> {
    let stdout = run_nmcli(
        shell,
        &[
            "-t",
            "-f",
            "DEVICE,TYPE,STATE,CONNECTION",
            "device",
            "status",
        ],
    )
    .await?;
    parse_devices(&stdout)
}

async fn run_nmcli(shell: &mut dyn LinuxShell, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("nmcli");
    for a in args {
        cmd = cmd.arg(*a);
    }
    let r = shell.run(&cmd).await?;
    Ok(r.stdout().unwrap_or("").to_string())
}

fn parse_connections(stdout: &str) -> Result<Vec<Connection>> {
    let mut out = Vec::new();
    for (lineno, line) in stdout.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = parse_terse_line(line);
        if fields.len() != 4 {
            return Err(Error::Parse(format!(
                "nmcli connection show: expected 4 fields on line {}, got {}: {line:?}",
                lineno + 1,
                fields.len()
            )));
        }
        let mut iter = fields.into_iter();
        let name = iter.next().unwrap();
        let uuid = iter.next().unwrap();
        let kind = iter.next().unwrap();
        let device = iter.next().unwrap();
        out.push(Connection {
            name,
            uuid,
            kind,
            device: empty_to_none(device),
        });
    }
    Ok(out)
}

fn parse_devices(stdout: &str) -> Result<Vec<Device>> {
    let mut out = Vec::new();
    for (lineno, line) in stdout.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = parse_terse_line(line);
        if fields.len() != 4 {
            return Err(Error::Parse(format!(
                "nmcli device status: expected 4 fields on line {}, got {}: {line:?}",
                lineno + 1,
                fields.len()
            )));
        }
        let mut iter = fields.into_iter();
        let name = iter.next().unwrap();
        let kind = iter.next().unwrap();
        let state = iter.next().unwrap();
        let connection = iter.next().unwrap();
        out.push(Device {
            name,
            kind,
            state,
            connection: empty_to_none(connection),
        });
    }
    Ok(out)
}

/// Parse one line of nmcli `-t` (terse) output into its fields.
///
/// `:` is the field separator. Literal colons inside a value are
/// escaped as `\:` and literal backslashes as `\\`. The parser
/// undoes both escapes; any other `\<x>` is left as-is (defensive
/// against future schema additions).
fn parse_terse_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some(':') | Some('\\') => {
                    current.push(chars.next().unwrap());
                }
                _ => current.push(c),
            },
            ':' => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

fn empty_to_none(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONNECTIONS_OUTPUT: &str = "\
Wired connection 1:abc-def-123:802-3-ethernet:eth0
Home WiFi:xyz-789-456:802-11-wireless:wlan0
Cafe\\: somewhere:cafe-uuid:802-11-wireless:
";

    const DEVICES_OUTPUT: &str = "\
eth0:ethernet:connected:Wired connection 1
wlan0:wifi:disconnected:
lo:loopback:connected (externally):lo
sit0:iptunnel:unmanaged:
";

    #[test]
    fn parses_connections() {
        let cs = parse_connections(CONNECTIONS_OUTPUT).unwrap();
        assert_eq!(cs.len(), 3);

        assert_eq!(cs[0].name, "Wired connection 1");
        assert_eq!(cs[0].uuid, "abc-def-123");
        assert_eq!(cs[0].kind, "802-3-ethernet");
        assert_eq!(cs[0].device.as_deref(), Some("eth0"));
        assert!(cs[0].is_active());

        assert_eq!(cs[1].name, "Home WiFi");
        assert!(cs[1].is_active());

        // Connection name with an escaped colon — should be unescaped.
        assert_eq!(cs[2].name, "Cafe: somewhere");
        assert_eq!(cs[2].device, None);
        assert!(!cs[2].is_active());
    }

    #[test]
    fn parses_devices() {
        let ds = parse_devices(DEVICES_OUTPUT).unwrap();
        assert_eq!(ds.len(), 4);

        assert_eq!(ds[0].name, "eth0");
        assert_eq!(ds[0].kind, "ethernet");
        assert_eq!(ds[0].state, "connected");
        assert_eq!(ds[0].connection.as_deref(), Some("Wired connection 1"));
        assert!(ds[0].is_connected());

        assert_eq!(ds[1].name, "wlan0");
        assert!(!ds[1].is_connected());
        assert!(ds[1].connection.is_none());

        // `connected (externally)` — NM didn't bring it up but the
        // device is connected. is_connected() must still be true.
        assert_eq!(ds[2].state, "connected (externally)");
        assert!(
            ds[2].is_connected(),
            "externally-connected still counts as connected"
        );

        assert_eq!(ds[3].state, "unmanaged");
        assert!(!ds[3].is_connected());
    }

    #[test]
    fn parses_terse_line_unescapes_colon_and_backslash() {
        let fields = parse_terse_line(r"name with\: colon:second:third\\with\\backslashes");
        assert_eq!(
            fields,
            vec!["name with: colon", "second", r"third\with\backslashes",]
        );
    }

    #[test]
    fn parses_terse_line_handles_empty_fields() {
        let fields = parse_terse_line("a::c");
        assert_eq!(fields, vec!["a", "", "c"]);
    }

    #[test]
    fn rejects_malformed_lines() {
        // Only three fields where we expect four.
        let err = parse_connections("only:three:fields\n").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    fn host_has_nmcli() -> bool {
        std::process::Command::new("sh")
            .args(["-c", "nmcli -t -f NAME connection show >/dev/null 2>&1"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn connections_via_subprocess_shell() {
        if !host_has_nmcli() {
            eprintln!("skipping: host has no working nmcli");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        let cs = connections(&mut shell).await.unwrap();
        eprintln!("[test] {} connections on host", cs.len());
    }

    #[tokio::test]
    async fn devices_via_subprocess_shell() {
        if !host_has_nmcli() {
            eprintln!("skipping: host has no working nmcli");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        let ds = devices(&mut shell).await.unwrap();
        eprintln!("[test] {} devices on host", ds.len());
        // Every host has loopback. NM may or may not manage it, but
        // `lo` appears in `nmcli device status`.
        assert!(ds.iter().any(|d| d.name == "lo"));
    }
}
