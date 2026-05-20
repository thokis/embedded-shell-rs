//! Wrappers around `journalctl` for reading the systemd journal.
//!
//! Gated by the `systemd` Cargo feature (the same one as the
//! [`crate::systemd`] module) because `journalctl` ships with
//! systemd — any device that has one has the other.
//!
//! # Device-side requirement
//!
//! `journalctl` on the device's `PATH`, emitting `-o json` (any
//! reasonably recent systemd does). The current user must have read
//! access to the journal: typically root, or membership in the
//! `systemd-journal` group, or the device having
//! `Storage=persistent` + `permissions: world-readable` configured.
//!
//! # Surface
//!
//! Read-only:
//!
//! - [`tail`] — the last *N* journal entries, regardless of source.
//! - [`tail_unit`] — the last *N* entries from one systemd unit.
//!
//! That's it for v1. Follow-mode (`journalctl -f`), time-window
//! filtering (`--since` / `--until`), and priority filtering aren't
//! exposed yet — the API design for streaming and structured filter
//! arguments deserves its own thought when there's a concrete use
//! case. For one-off queries that don't fit, drop into
//! `shell.run(Command::new("journalctl").args([...]))` directly and
//! parse the output yourself.
//!
//! [`LinuxShell`]: embedded_shell::shell::LinuxShell

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use embedded_shell::shell::{Command, LinuxShell};
use serde::Deserialize;

use crate::error::{Error, Result};

/// syslog priority levels, as defined in RFC 5424. Lower numeric
/// values are more severe.
///
/// The repr matches the on-the-wire priority numbers journalctl
/// emits (and that you'd pass to `journalctl -p`), so casting to
/// `u8` round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Priority {
    Emergency = 0,
    Alert = 1,
    Critical = 2,
    Error = 3,
    Warning = 4,
    Notice = 5,
    Info = 6,
    Debug = 7,
}

impl Priority {
    /// Map a 0–7 priority number to the enum variant. Returns
    /// `None` for out-of-range input.
    pub fn from_u8(n: u8) -> Option<Self> {
        Some(match n {
            0 => Self::Emergency,
            1 => Self::Alert,
            2 => Self::Critical,
            3 => Self::Error,
            4 => Self::Warning,
            5 => Self::Notice,
            6 => Self::Info,
            7 => Self::Debug,
            _ => return None,
        })
    }

    /// Numeric priority — `0` (most severe) through `7` (least).
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// One entry from the systemd journal.
///
/// Captures the fields that most callers actually want; raw
/// journalctl entries can have hundreds of additional fields (every
/// `KEY=value` line a service logged) but we ignore them in v1. If
/// you need them, drop into a raw `shell.run(journalctl ...)` call
/// and parse the JSON yourself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Wall-clock time the entry was logged, parsed from
    /// `__REALTIME_TIMESTAMP` (microseconds since the Unix epoch).
    pub timestamp: SystemTime,
    /// syslog priority, or `None` when the entry doesn't carry one
    /// (some kernel-emitted entries omit it).
    pub priority: Option<Priority>,
    /// systemd unit that emitted the entry, e.g. `sshd.service`.
    /// `None` for kernel logs and entries from outside systemd.
    pub unit: Option<String>,
    /// The actual log message.
    pub message: String,
    /// Host that emitted the entry. Useful when querying via remote
    /// journal collection; otherwise it's just the device's
    /// hostname.
    pub hostname: Option<String>,
    /// PID of the process that logged the entry.
    pub pid: Option<u32>,
    /// The logging program's identifier: `SYSLOG_IDENTIFIER` if
    /// set, falling back to `_COMM` (the process's `comm` name).
    pub identifier: Option<String>,
}

/// Returns the last `count` journal entries, newest last.
///
/// Equivalent to `journalctl -o json --no-pager -n <count>`, then
/// parsed and reversed so the returned `Vec` is in chronological
/// order (matching the on-screen order you'd see in a terminal).
///
/// # Errors
///
/// - [`Error::Shell`] if `journalctl` isn't installed or exits
///   non-zero (most commonly: permission denied because the caller
///   isn't in the `systemd-journal` group).
/// - [`Error::Parse`] if a returned line isn't valid JSON, or is
///   valid JSON but doesn't contain `__REALTIME_TIMESTAMP` or
///   `MESSAGE` (extremely unlikely).
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::journalctl;
///
/// for entry in journalctl::tail(&mut shell, 50).await? {
///     println!("{:?} {}: {}", entry.timestamp, entry.unit.as_deref().unwrap_or("-"), entry.message);
/// }
/// ```
pub async fn tail(shell: &mut dyn LinuxShell, count: u32) -> Result<Vec<LogEntry>> {
    let output = run_journalctl_json(shell, &["-n", &count.to_string()]).await?;
    parse_json_seq(&output)
}

/// Returns the last `count` journal entries from one systemd unit,
/// newest last.
///
/// Equivalent to `journalctl -o json --no-pager -u <unit> -n <count>`.
///
/// # Errors
///
/// As for [`tail`].
///
/// # Example
///
/// ```ignore
/// let recent = journalctl::tail_unit(&mut shell, "sshd.service", 20).await?;
/// for entry in recent {
///     println!("{}", entry.message);
/// }
/// ```
pub async fn tail_unit(
    shell: &mut dyn LinuxShell,
    unit: &str,
    count: u32,
) -> Result<Vec<LogEntry>> {
    let output = run_journalctl_json(shell, &["-u", unit, "-n", &count.to_string()]).await?;
    parse_json_seq(&output)
}

/// Returns every journal entry since `since`, regardless of source.
///
/// `since` is passed verbatim to `journalctl --since` and accepts
/// every form it does: absolute timestamps (`"2024-01-15 09:00:00"`,
/// `"2024-01-15"`), relative expressions (`"1 hour ago"`, `"yesterday"`,
/// `"-15m"`), or the special tokens `"now"` and `"today"`.
///
/// Equivalent to `journalctl -o json --no-pager --since=<since>`.
///
/// # Errors
///
/// As for [`tail`], plus a [`Error::Shell`] for malformed `since`
/// expressions that journalctl rejects.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::journalctl;
///
/// // Everything since the last reboot's worth of activity.
/// let recent = journalctl::tail_since(&mut shell, "1 hour ago").await?;
/// ```
pub async fn tail_since(shell: &mut dyn LinuxShell, since: &str) -> Result<Vec<LogEntry>> {
    let output = run_journalctl_json(shell, &["--since", since]).await?;
    parse_json_seq(&output)
}

/// Returns journal entries from one systemd unit since `since`.
///
/// Combines the filters of [`tail_unit`] and [`tail_since`].
/// Equivalent to `journalctl -o json --no-pager -u <unit> --since=<since>`.
///
/// # Errors
///
/// As for [`tail_since`].
///
/// # Example
///
/// ```ignore
/// let entries = journalctl::tail_unit_since(&mut shell, "sshd.service", "yesterday").await?;
/// ```
pub async fn tail_unit_since(
    shell: &mut dyn LinuxShell,
    unit: &str,
    since: &str,
) -> Result<Vec<LogEntry>> {
    let output = run_journalctl_json(shell, &["-u", unit, "--since", since]).await?;
    parse_json_seq(&output)
}

async fn run_journalctl_json(shell: &mut dyn LinuxShell, extra_args: &[&str]) -> Result<String> {
    // `-o json-seq` is RFC 7464: each record prefixed with `\x1e`
    // (record separator) and terminated by `\n`. Critically it
    // *does* escape newlines inside string values — `-o json` does
    // not, which breaks line-based parsing on multi-line log
    // messages (e.g. pretty-printed Debug output that spans
    // several lines).
    let mut cmd = Command::new("journalctl")
        .arg("-o")
        .arg("json-seq")
        .arg("--no-pager");
    for a in extra_args {
        cmd = cmd.arg(*a);
    }
    let r = shell.run(&cmd).await?;
    Ok(r.stdout().unwrap_or("").to_string())
}

fn parse_json_seq(output: &str) -> Result<Vec<LogEntry>> {
    // Records separated by 0x1e (Record Separator). The first split
    // chunk is empty (output starts with \x1e); subsequent chunks
    // each contain one JSON object followed by an optional newline.
    let mut out = Vec::new();
    for record in output.split('\x1e') {
        let record = record.trim();
        if record.is_empty() {
            continue;
        }
        let raw: RawEntry = serde_json::from_str(record)
            .map_err(|e| Error::Parse(format!("journalctl json: {e}; record {record:?}")))?;
        out.push(LogEntry::from_raw(raw)?);
    }
    Ok(out)
}

// ---------- internal JSON shape ----------

#[derive(Deserialize)]
struct RawEntry {
    #[serde(rename = "__REALTIME_TIMESTAMP")]
    realtime_timestamp: Option<String>,
    #[serde(rename = "PRIORITY")]
    priority: Option<String>,
    #[serde(rename = "_SYSTEMD_UNIT")]
    systemd_unit: Option<String>,
    #[serde(rename = "MESSAGE")]
    message: Option<serde_json::Value>,
    #[serde(rename = "_HOSTNAME")]
    hostname: Option<String>,
    #[serde(rename = "_PID")]
    pid: Option<String>,
    #[serde(rename = "SYSLOG_IDENTIFIER")]
    syslog_identifier: Option<String>,
    #[serde(rename = "_COMM")]
    comm: Option<String>,
}

impl LogEntry {
    fn from_raw(raw: RawEntry) -> Result<Self> {
        let micros: u64 = raw
            .realtime_timestamp
            .as_deref()
            .ok_or_else(|| Error::Parse("journal entry missing __REALTIME_TIMESTAMP".into()))?
            .parse()
            .map_err(|e| Error::Parse(format!("__REALTIME_TIMESTAMP not numeric: {e}")))?;
        let timestamp = UNIX_EPOCH + Duration::from_micros(micros);

        let priority = raw
            .priority
            .as_deref()
            .and_then(|p| p.parse::<u8>().ok())
            .and_then(Priority::from_u8);

        let pid = raw.pid.as_deref().and_then(|p| p.parse::<u32>().ok());

        let message = match raw.message {
            Some(serde_json::Value::String(s)) => s,
            // Non-UTF-8 messages serialize as an array of byte integers.
            // Best-effort lossy decode.
            Some(serde_json::Value::Array(bytes)) => {
                let buf: Vec<u8> = bytes
                    .into_iter()
                    .filter_map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
                    .collect();
                String::from_utf8_lossy(&buf).into_owned()
            }
            // Missing or weird shape — treat as empty rather than
            // failing, since this happens occasionally for entries
            // without a real message body.
            _ => String::new(),
        };

        let identifier = raw.syslog_identifier.or(raw.comm);

        Ok(Self {
            timestamp,
            priority,
            unit: raw.systemd_unit,
            message,
            hostname: raw.hostname,
            pid,
            identifier,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `-o json-seq` framing: each record prefixed with 0x1e and
    // terminated by 0x0a (RFC 7464).
    const SAMPLE_JSON_SEQ: &str = concat!(
        "\x1e",
        r#"{"__REALTIME_TIMESTAMP":"1700000000123456","PRIORITY":"6","_SYSTEMD_UNIT":"sshd.service","_HOSTNAME":"router","_PID":"1234","SYSLOG_IDENTIFIER":"sshd","_COMM":"sshd","MESSAGE":"Accepted publickey for thomas"}"#,
        "\n\x1e",
        r#"{"__REALTIME_TIMESTAMP":"1700000001000000","PRIORITY":"3","_SYSTEMD_UNIT":"foo.service","_HOSTNAME":"router","MESSAGE":"connection refused"}"#,
        "\n\x1e",
        r#"{"__REALTIME_TIMESTAMP":"1700000002500000","_HOSTNAME":"router","MESSAGE":"kernel: net eth0 link up"}"#,
        "\n",
    );

    #[test]
    fn parses_json_seq_output() {
        let entries = parse_json_seq(SAMPLE_JSON_SEQ).unwrap();
        assert_eq!(entries.len(), 3);

        let sshd = &entries[0];
        assert_eq!(sshd.message, "Accepted publickey for thomas");
        assert_eq!(sshd.priority, Some(Priority::Info));
        assert_eq!(sshd.unit.as_deref(), Some("sshd.service"));
        assert_eq!(sshd.pid, Some(1234));
        assert_eq!(sshd.identifier.as_deref(), Some("sshd"));

        let foo = &entries[1];
        assert_eq!(foo.priority, Some(Priority::Error));

        // Kernel-style entry: no _SYSTEMD_UNIT, no PRIORITY, no PID.
        let kernel = &entries[2];
        assert!(kernel.unit.is_none());
        assert!(kernel.priority.is_none());
        assert!(kernel.pid.is_none());
        assert_eq!(kernel.message, "kernel: net eth0 link up");
    }

    #[test]
    fn timestamps_round_trip_via_unix_epoch() {
        let entries = parse_json_seq(SAMPLE_JSON_SEQ).unwrap();
        let first = &entries[0];
        let dur = first.timestamp.duration_since(UNIX_EPOCH).unwrap();
        // 1700000000123456 microseconds since epoch
        assert_eq!(dur.as_secs(), 1_700_000_000);
        assert_eq!(dur.subsec_micros(), 123_456);
    }

    #[test]
    fn priority_ordering_is_severity() {
        // Lower numeric values = more severe.
        assert!(Priority::Error < Priority::Warning);
        assert!(Priority::Emergency < Priority::Alert);
        assert!(Priority::Debug > Priority::Info);
    }

    #[test]
    fn priority_round_trips_through_u8() {
        for n in 0u8..=7 {
            assert_eq!(Priority::from_u8(n).unwrap().as_u8(), n);
        }
        assert!(Priority::from_u8(8).is_none());
        assert!(Priority::from_u8(255).is_none());
    }

    #[test]
    fn handles_message_as_byte_array() {
        let input =
            "\x1e{\"__REALTIME_TIMESTAMP\":\"1700000000000000\",\"MESSAGE\":[104,105,33]}\n";
        let entries = parse_json_seq(input).unwrap();
        assert_eq!(entries[0].message, "hi!");
    }

    #[test]
    fn rejects_garbage_timestamp() {
        let input = "\x1e{\"__REALTIME_TIMESTAMP\":\"not-a-number\",\"MESSAGE\":\"foo\"}\n";
        let err = parse_json_seq(input).unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    #[test]
    fn skips_empty_records() {
        // Leading newlines and a trailing RS-without-payload should
        // all be tolerated.
        let input = format!("\n\n{SAMPLE_JSON_SEQ}\x1e");
        let entries = parse_json_seq(&input).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn parses_message_with_escaped_newline() {
        // Regression: `-o json-seq` correctly escapes literal newlines
        // inside string values as `\n`, so multi-line log messages
        // (e.g. pretty-printed Debug output) parse cleanly. The old
        // `-o json` format emitted raw \x0a inside the string and
        // broke line-based parsing.
        let input = "\x1e{\"__REALTIME_TIMESTAMP\":\"1700000000000000\",\
                     \"MESSAGE\":\"State change ConnectionState {\\n    state: Connected,\\n    addr: 1.2.3.4,\\n}\"}\n";
        let entries = parse_json_seq(input).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].message.contains("State change"));
        assert!(entries[0].message.contains("Connected"));
        // Verify the embedded newlines came through.
        assert_eq!(entries[0].message.lines().count(), 4);
    }

    fn host_can_read_journal() -> bool {
        std::process::Command::new("sh")
            .args(["-c", "journalctl -o json -n 1 >/dev/null 2>&1"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn tail_via_subprocess_shell() {
        if !host_can_read_journal() {
            eprintln!("skipping: host can't read journal (no journalctl, or no permission)");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        let entries = tail(&mut shell, 5).await.unwrap();
        eprintln!("[test] {} entries", entries.len());
        // We don't assert on count > 0: a freshly-booted host might
        // have very few accessible entries. But the call should
        // have parsed cleanly.
        for entry in &entries {
            assert!(
                entry.timestamp.duration_since(UNIX_EPOCH).is_ok(),
                "every entry has a valid timestamp"
            );
        }
    }

    #[tokio::test]
    async fn tail_since_via_subprocess_shell() {
        if !host_can_read_journal() {
            eprintln!("skipping: host can't read journal");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        // Anything within the last hour. May be empty (idle host) but
        // must parse cleanly.
        let entries = tail_since(&mut shell, "1 hour ago").await.unwrap();
        eprintln!("[test] {} entries since 1 hour ago", entries.len());
        let one_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for entry in &entries {
            assert!(
                entry.timestamp >= one_hour_ago,
                "entry timestamp before the window: {:?}",
                entry.timestamp
            );
        }
    }
}
