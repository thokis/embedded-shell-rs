//! Wrappers around `iputils` / busybox network tools.
//!
//! Exposes [`ping`] (ICMP reachability) and [`arping`] (ARP-layer
//! reachability on the local network segment). Enabled by the
//! `iputils` Cargo feature (default-on).
//!
//! # Device-side requirements
//!
//! A `ping` binary for [`ping`], an `arping` binary for [`arping`].
//! Both GNU iputils and busybox variants are accepted — the output
//! parsers handle both dialects.
//!
//! [`LinuxShell`]: embedded_shell::shell::LinuxShell

use std::sync::OnceLock;
use std::time::Duration;

use embedded_shell::shell::{Command, LinuxShell};
use regex::Regex;

use crate::error::{Error, Result};

/// Result of a [`ping`] call. Mirrors the summary `ping` itself prints.
#[derive(Debug, Clone, PartialEq)]
pub struct PingStats {
    /// Number of ICMP echo-request packets sent.
    pub transmitted: u32,
    /// Number of ICMP echo-reply packets received.
    pub received: u32,
    /// Packet-loss percentage as reported by `ping` (`(transmitted -
    /// received) / transmitted * 100`, rounded by the tool itself).
    pub loss_percent: f32,
    /// Minimum round-trip time in milliseconds, or `None` when no
    /// packets returned.
    pub rtt_min_ms: Option<f32>,
    /// Average round-trip time in milliseconds, or `None` when no
    /// packets returned.
    pub rtt_avg_ms: Option<f32>,
    /// Maximum round-trip time in milliseconds, or `None` when no
    /// packets returned.
    pub rtt_max_ms: Option<f32>,
}

impl PingStats {
    /// `true` if at least one echo-reply arrived. Equivalent to
    /// `self.received > 0`.
    pub fn is_reachable(&self) -> bool {
        self.received > 0
    }
}

/// Send `count` ICMP echo requests to `target` and return the summary
/// statistics.
///
/// `target` is passed verbatim to the device's `ping` and may be an
/// IPv4 address, an IPv6 address, or a hostname (DNS resolution happens
/// on the device).
///
/// The per-packet response timeout (`ping -W`) is fixed at 1 second.
/// The total host-side timeout is `count + 5` seconds.
///
/// # Errors
///
/// - [`Error::Shell`] wrapping
///   [`ShellError::CommandNotFound`][embedded_shell::shell::ShellError::CommandNotFound]
///   if `ping` isn't installed on the device.
/// - [`Error::Shell`] wrapping
///   [`ShellError::CommandFailed`][embedded_shell::shell::ShellError::CommandFailed]
///   if `ping` exits for a reason other than packet loss (bad
///   hostname, no network route, …).
/// - [`Error::Parse`] if the device's `ping` produced output the parser
///   can't interpret.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::iputils;
///
/// let stats = iputils::ping(&mut shell, "8.8.8.8", 3).await?;
/// if stats.is_reachable() {
///     println!("avg latency: {:.1} ms", stats.rtt_avg_ms.unwrap_or(0.0));
/// }
/// ```
pub async fn ping(shell: &mut dyn LinuxShell, target: &str, count: u32) -> Result<PingStats> {
    let cmd = Command::new("ping")
        .arg("-c")
        .arg(count.to_string())
        .arg("-W")
        .arg("1")
        .arg(target)
        .timeout(Duration::from_secs(u64::from(count) + 5))
        .allow_nonzero();
    let result = shell.run(&cmd).await?;
    let stdout = result.stdout().unwrap_or("");
    parse_ping_output(stdout)
}

fn stats_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(\d+) packets transmitted, (\d+)(?: packets)? received, ([\d.]+)% packet loss")
            .expect("ping stats regex is valid")
    })
}

fn rtt_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:round-trip|rtt) min/avg/max(?:/mdev)? = ([\d.]+)/([\d.]+)/([\d.]+)(?:/[\d.]+)? ms",
        )
        .expect("ping rtt regex is valid")
    })
}

fn parse_ping_output(stdout: &str) -> Result<PingStats> {
    let stats_caps = stats_regex().captures(stdout).ok_or_else(|| {
        Error::Parse(format!(
            "could not find ping summary line in output: {stdout:?}"
        ))
    })?;
    let transmitted = stats_caps[1]
        .parse::<u32>()
        .map_err(|e| Error::Parse(format!("transmitted: {e}")))?;
    let received = stats_caps[2]
        .parse::<u32>()
        .map_err(|e| Error::Parse(format!("received: {e}")))?;
    let loss_percent = stats_caps[3]
        .parse::<f32>()
        .map_err(|e| Error::Parse(format!("loss_percent: {e}")))?;

    let (rtt_min_ms, rtt_avg_ms, rtt_max_ms) = match rtt_regex().captures(stdout) {
        Some(rtt_caps) => (
            rtt_caps[1].parse::<f32>().ok(),
            rtt_caps[2].parse::<f32>().ok(),
            rtt_caps[3].parse::<f32>().ok(),
        ),
        None => (None, None, None),
    };

    Ok(PingStats {
        transmitted,
        received,
        loss_percent,
        rtt_min_ms,
        rtt_avg_ms,
        rtt_max_ms,
    })
}

/// Result of an [`arping`] call.
///
/// Mirrors the summary `arping` itself prints, plus the discovered
/// peer MAC address when at least one reply arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArpingStats {
    /// Number of ARP probes sent.
    pub sent: u32,
    /// Number of replies received.
    pub received: u32,
    /// MAC address of the responder, if any reply was received. The
    /// format is whatever `arping` printed — typically lowercase
    /// colon-separated (`aa:bb:cc:dd:ee:ff`).
    pub target_mac: Option<String>,
}

impl ArpingStats {
    /// `true` if at least one reply arrived. Equivalent to
    /// `self.received > 0`.
    pub fn is_reachable(&self) -> bool {
        self.received > 0
    }
}

/// Send `count` ARP probes to `target` and return the summary.
///
/// Unlike [`ping`], which works on any IP, `arping` operates at the
/// link layer and only sees hosts on the **same local network
/// segment** as the device. It's the right tool for "is this IP live
/// on the LAN right now?" — independent of routing and not blocked
/// by ICMP firewalls.
///
/// `target` is passed verbatim to the device's `arping` and is
/// typically an IPv4 address. Hostnames work iff `arping` resolves
/// them; behavior varies by implementation.
///
/// The per-probe response timeout (`arping -w`) is fixed at 1
/// second. The total host-side timeout is `count + 5` seconds.
///
/// # Device-side notes
///
/// - `arping` usually requires `CAP_NET_RAW` (i.e. root or a setuid
///   binary). Busybox `arping` is typically setuid; iputils `arping`
///   sometimes isn't on minimal Linux distros.
/// - No interface (`-I`) is passed — `arping` auto-selects based on
///   the route to `target`. On multi-interface devices, set the
///   default route appropriately or use a hostname that resolves
///   over the right route.
///
/// # Errors
///
/// - [`Error::Shell`] wrapping
///   [`ShellError::CommandNotFound`][embedded_shell::shell::ShellError::CommandNotFound]
///   if `arping` isn't installed on the device.
/// - [`Error::Shell`] wrapping
///   [`ShellError::CommandFailed`][embedded_shell::shell::ShellError::CommandFailed]
///   if `arping` exits non-zero for a reason other than packet loss
///   (no route to target, permission denied, …).
/// - [`Error::Parse`] if the device's `arping` produced output the
///   parser can't interpret.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::iputils;
///
/// let stats = iputils::arping(&mut shell, "192.168.1.1", 3).await?;
/// if let Some(mac) = &stats.target_mac {
///     println!("gateway MAC: {mac}");
/// }
/// ```
pub async fn arping(shell: &mut dyn LinuxShell, target: &str, count: u32) -> Result<ArpingStats> {
    let cmd = Command::new("arping")
        .arg("-c")
        .arg(count.to_string())
        .arg("-w")
        .arg("1")
        .arg(target)
        .timeout(Duration::from_secs(u64::from(count) + 5))
        .allow_nonzero();
    let result = shell.run(&cmd).await?;
    let stdout = result.stdout().unwrap_or("");
    parse_arping_output(stdout)
}

fn arping_sent_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // iputils: "Sent 3 probes (1 broadcast(s))"
        // busybox: "Sent 3 probe(s) (1 broadcast(s))"
        Regex::new(r"Sent (\d+) probe").expect("arping sent regex is valid")
    })
}

fn arping_received_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // iputils: "Received 3 response(s)"
        // busybox: "Received 3 reply (0 request(s), 0 broadcast(s))"
        Regex::new(r"Received (\d+) (?:reply|response)").expect("arping received regex is valid")
    })
}

fn arping_mac_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Matches the bracketed MAC in any "Unicast reply from … [aa:bb:…]" line.
        Regex::new(r"\[([0-9a-fA-F]{2}(?::[0-9a-fA-F]{2}){5})\]")
            .expect("arping mac regex is valid")
    })
}

fn parse_arping_output(stdout: &str) -> Result<ArpingStats> {
    let sent_caps = arping_sent_regex().captures(stdout).ok_or_else(|| {
        Error::Parse(format!(
            "could not find arping `Sent` summary line in output: {stdout:?}"
        ))
    })?;
    let received_caps = arping_received_regex().captures(stdout).ok_or_else(|| {
        Error::Parse(format!(
            "could not find arping `Received` summary line in output: {stdout:?}"
        ))
    })?;
    let sent = sent_caps[1]
        .parse::<u32>()
        .map_err(|e| Error::Parse(format!("arping sent: {e}")))?;
    let received = received_caps[1]
        .parse::<u32>()
        .map_err(|e| Error::Parse(format!("arping received: {e}")))?;

    let target_mac = arping_mac_regex()
        .captures(stdout)
        .map(|c| c[1].to_lowercase());

    Ok(ArpingStats {
        sent,
        received,
        target_mac,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GNU_OUTPUT: &str = "\
PING 8.8.8.8 (8.8.8.8) 56(84) bytes of data.
64 bytes from 8.8.8.8: icmp_seq=1 ttl=119 time=11.2 ms
64 bytes from 8.8.8.8: icmp_seq=2 ttl=119 time=11.0 ms
64 bytes from 8.8.8.8: icmp_seq=3 ttl=119 time=11.5 ms

--- 8.8.8.8 ping statistics ---
3 packets transmitted, 3 received, 0% packet loss, time 2003ms
rtt min/avg/max/mdev = 10.987/11.225/11.456/0.234 ms
";

    const BUSYBOX_OUTPUT: &str = "\
PING 8.8.8.8 (8.8.8.8): 56 data bytes
64 bytes from 8.8.8.8: seq=0 ttl=119 time=11.234 ms
64 bytes from 8.8.8.8: seq=1 ttl=119 time=10.987 ms
64 bytes from 8.8.8.8: seq=2 ttl=119 time=11.456 ms

--- 8.8.8.8 ping statistics ---
3 packets transmitted, 3 packets received, 0% packet loss
round-trip min/avg/max = 10.987/11.225/11.456 ms
";

    const ALL_LOST_OUTPUT: &str = "\
PING 192.0.2.1 (192.0.2.1) 56(84) bytes of data.

--- 192.0.2.1 ping statistics ---
3 packets transmitted, 0 received, 100% packet loss, time 2014ms
";

    #[test]
    fn parses_gnu_iputils_output() {
        let stats = parse_ping_output(GNU_OUTPUT).unwrap();
        assert_eq!(stats.transmitted, 3);
        assert_eq!(stats.received, 3);
        assert_eq!(stats.loss_percent, 0.0);
        assert_eq!(stats.rtt_min_ms, Some(10.987));
        assert_eq!(stats.rtt_avg_ms, Some(11.225));
        assert_eq!(stats.rtt_max_ms, Some(11.456));
        assert!(stats.is_reachable());
    }

    #[test]
    fn parses_busybox_output() {
        let stats = parse_ping_output(BUSYBOX_OUTPUT).unwrap();
        assert_eq!(stats.transmitted, 3);
        assert_eq!(stats.received, 3);
        assert_eq!(stats.loss_percent, 0.0);
        assert_eq!(stats.rtt_min_ms, Some(10.987));
        assert_eq!(stats.rtt_avg_ms, Some(11.225));
        assert_eq!(stats.rtt_max_ms, Some(11.456));
        assert!(stats.is_reachable());
    }

    #[test]
    fn parses_total_loss_output() {
        let stats = parse_ping_output(ALL_LOST_OUTPUT).unwrap();
        assert_eq!(stats.transmitted, 3);
        assert_eq!(stats.received, 0);
        assert_eq!(stats.loss_percent, 100.0);
        assert_eq!(stats.rtt_min_ms, None);
        assert_eq!(stats.rtt_avg_ms, None);
        assert_eq!(stats.rtt_max_ms, None);
        assert!(!stats.is_reachable());
    }

    #[test]
    fn unparseable_output_yields_parse_error() {
        let err = parse_ping_output("ping: bad target: Name or service not known\n").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    fn host_has_ping() -> bool {
        std::process::Command::new("sh")
            .args(["-c", "command -v ping"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn host_can_ping_loopback() -> bool {
        std::process::Command::new("sh")
            .args(["-c", "ping -c 1 -W 1 127.0.0.1 >/dev/null 2>&1"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn ping_loopback_via_subprocess_shell() {
        if !host_has_ping() || !host_can_ping_loopback() {
            eprintln!("skipping: host can't ping 127.0.0.1 unprivileged");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        let stats = ping(&mut shell, "127.0.0.1", 2).await.unwrap();
        assert!(stats.is_reachable(), "loopback unreachable: {stats:?}");
        assert_eq!(stats.transmitted, 2);
        assert_eq!(stats.received, 2);
        assert_eq!(stats.loss_percent, 0.0);
        assert!(stats.rtt_avg_ms.is_some());
    }

    // ---------- arping ----------

    const ARPING_IPUTILS_OUTPUT: &str = "\
ARPING 192.168.1.1 from 192.168.1.5 wlan0
Unicast reply from 192.168.1.1 [aa:bb:cc:dd:ee:ff]  1.234ms
Unicast reply from 192.168.1.1 [aa:bb:cc:dd:ee:ff]  1.105ms
Unicast reply from 192.168.1.1 [aa:bb:cc:dd:ee:ff]  0.987ms
Sent 3 probes (1 broadcast(s))
Received 3 response(s)
";

    const ARPING_BUSYBOX_OUTPUT: &str = "\
ARPING to 192.168.1.1 from 192.168.1.5 via wlan0
Unicast reply from 192.168.1.1 [AA:BB:CC:DD:EE:FF] 1.234ms
Unicast reply from 192.168.1.1 [AA:BB:CC:DD:EE:FF] 1.105ms
Unicast reply from 192.168.1.1 [AA:BB:CC:DD:EE:FF] 0.987ms
Sent 3 probe(s) (1 broadcast(s))
Received 3 reply (0 request(s), 0 broadcast(s))
";

    const ARPING_TOTAL_LOSS_OUTPUT: &str = "\
ARPING 192.0.2.1 from 192.168.1.5 wlan0
Sent 3 probes (3 broadcast(s))
Received 0 response(s)
";

    #[test]
    fn parses_iputils_arping_output() {
        let stats = parse_arping_output(ARPING_IPUTILS_OUTPUT).unwrap();
        assert_eq!(stats.sent, 3);
        assert_eq!(stats.received, 3);
        assert_eq!(stats.target_mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        assert!(stats.is_reachable());
    }

    #[test]
    fn parses_busybox_arping_output_and_lowercases_mac() {
        let stats = parse_arping_output(ARPING_BUSYBOX_OUTPUT).unwrap();
        assert_eq!(stats.sent, 3);
        assert_eq!(stats.received, 3);
        // Both implementations may emit uppercase or lowercase; the
        // parser canonicalises to lowercase.
        assert_eq!(stats.target_mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        assert!(stats.is_reachable());
    }

    #[test]
    fn parses_arping_total_loss_with_no_mac() {
        let stats = parse_arping_output(ARPING_TOTAL_LOSS_OUTPUT).unwrap();
        assert_eq!(stats.sent, 3);
        assert_eq!(stats.received, 0);
        assert_eq!(stats.target_mac, None);
        assert!(!stats.is_reachable());
    }

    #[test]
    fn unparseable_arping_output_yields_parse_error() {
        let err = parse_arping_output("arping: socket: Permission denied\n").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    /// Live arping test against whatever target is in
    /// `EMBEDDED_SHELL_ARPING_TARGET`. Skipped if the env var isn't
    /// set or arping isn't reachable — there's no portable "always
    /// works" arping target the way `127.0.0.1` is for ping (arping
    /// operates at the link layer and loopback has no ARP).
    #[tokio::test]
    async fn arping_via_subprocess_shell_against_env_target() {
        let Ok(target) = std::env::var("EMBEDDED_SHELL_ARPING_TARGET") else {
            eprintln!("skipping: set EMBEDDED_SHELL_ARPING_TARGET to a LAN IP to exercise this");
            return;
        };
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        match arping(&mut shell, &target, 2).await {
            Ok(stats) => {
                eprintln!("[test] arping({target}) -> {stats:?}");
                assert!(stats.sent >= 1);
            }
            Err(e) => {
                eprintln!("skipping: arping returned error (probably needs root): {e}");
            }
        }
    }
}
