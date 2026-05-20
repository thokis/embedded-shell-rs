//! Wrappers around iproute2's `ip` command: links, addresses, routes.
//!
//! Enabled by the opt-in `iproute2` Cargo feature, which also pulls
//! in `serde` and `serde_json` — the parser uses `ip -j` (JSON
//! output) rather than parsing free-form text.
//!
//! # Device-side requirement
//!
//! An `ip` binary that understands `-j` / JSON output. That's
//! iproute2 ≥ 4.0 (released 2015) and recent busybox builds with
//! `CONFIG_FEATURE_IP_JSON`. Older busybox `ip` doesn't emit JSON;
//! [`Error::Parse`] is returned on unparseable output.
//!
//! # Surface
//!
//! Read-only introspection only:
//!
//! - [`links`] — every network interface (`ip -j link show`).
//! - [`addresses`] — every IP address, flattened across interfaces
//!   (`ip -j addr show`).
//! - [`routes`] — every routing-table entry (`ip -j route show`).
//!
//! State-changing operations (`ip link set up`, `ip addr add`,
//! `ip route add`, …) aren't exposed in v1. They have a much bigger
//! risk profile — a wrong `ip link set down` can disconnect the
//! device permanently if you're operating over the network you're
//! about to take down — and the read paths cover the common
//! "what's the network state of this device?" question. Drop into
//! `shell.run(Command::new("ip").args(["link", "set", "eth0", "up"]))`
//! directly when you need mutation.
//!
//! [`LinuxShell`]: embedded_shell::shell::LinuxShell

use embedded_shell::shell::{Command, LinuxShell};
use serde::Deserialize;

use crate::error::{Error, Result};

/// A network interface (link), as returned by `ip link show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Kernel interface index.
    pub index: u32,
    /// Interface name (`lo`, `eth0`, `wlan0`, …).
    pub name: String,
    /// Operational state as reported by the driver: `UP`, `DOWN`,
    /// `UNKNOWN`, `DORMANT`, `LOWERLAYERDOWN`, …
    ///
    /// `UNKNOWN` is common and *not* an error — loopback and many
    /// virtual interfaces don't report state and show up as
    /// `UNKNOWN` while being fully functional. Use
    /// [`is_up`][Self::is_up] only when you specifically want the
    /// strict "definitely up" check.
    pub operstate: String,
    /// MTU in bytes.
    pub mtu: u32,
    /// MAC address (hex, colon-separated), or `None` for interfaces
    /// without one (some tunnels).
    pub mac: Option<String>,
}

impl Link {
    /// `true` only when [`operstate`][Self::operstate] is exactly
    /// `"UP"`. Returns `false` for loopback and virtual interfaces
    /// that report `UNKNOWN` — even though they may be functional.
    pub fn is_up(&self) -> bool {
        self.operstate == "UP"
    }
}

/// An IP address bound to an interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    /// Interface this address is bound to.
    pub interface: String,
    /// `inet` for IPv4, `inet6` for IPv6.
    pub family: String,
    /// The address itself, without prefix length.
    pub address: String,
    /// Prefix length (e.g. `24` for `/24`).
    pub prefix_len: u8,
    /// Scope: `global`, `link`, `host`, …
    pub scope: String,
}

impl Address {
    /// `true` for IPv4 (`family == "inet"`).
    pub fn is_ipv4(&self) -> bool {
        self.family == "inet"
    }
    /// `true` for IPv6 (`family == "inet6"`).
    pub fn is_ipv6(&self) -> bool {
        self.family == "inet6"
    }
}

/// A routing-table entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Destination — either a CIDR (`192.168.1.0/24`) or the literal
    /// string `"default"` for the default route (which would be
    /// `0.0.0.0/0` or `::/0` in CIDR form).
    pub destination: String,
    /// Next-hop gateway, if any. Link-local routes don't have one.
    pub gateway: Option<String>,
    /// Outgoing interface (`dev` field).
    pub interface: String,
    /// Routing metric (lower wins), if assigned.
    pub metric: Option<u32>,
    /// `link`, `host`, `global`, …
    pub scope: Option<String>,
    /// Routing protocol that installed this route: `kernel`,
    /// `static`, `dhcp`, `boot`, `ra`, …
    pub protocol: Option<String>,
}

/// Lists every network interface on the device.
///
/// # Errors
///
/// - [`Error::Shell`] if `ip` isn't installed or exits non-zero.
/// - [`Error::Parse`] if the output isn't parseable JSON (old
///   busybox without `CONFIG_FEATURE_IP_JSON`).
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::iproute2;
///
/// for link in iproute2::links(&mut shell).await? {
///     println!("{}: {} ({} bytes MTU)", link.name, link.operstate, link.mtu);
/// }
/// ```
pub async fn links(shell: &mut dyn LinuxShell) -> Result<Vec<Link>> {
    let json = run_ip_json(shell, &["-j", "link", "show"]).await?;
    let raw: Vec<RawLink> = serde_json::from_str(&json)
        .map_err(|e| Error::Parse(format!("ip -j link show: {e}; got {json:?}")))?;
    Ok(raw.into_iter().map(Link::from).collect())
}

/// Lists every IP address bound to any interface, flattened.
///
/// # Errors
///
/// As for [`links`].
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::iproute2;
///
/// let v4: Vec<_> = iproute2::addresses(&mut shell).await?
///     .into_iter()
///     .filter(|a| a.is_ipv4() && a.scope == "global")
///     .collect();
/// ```
pub async fn addresses(shell: &mut dyn LinuxShell) -> Result<Vec<Address>> {
    let json = run_ip_json(shell, &["-j", "addr", "show"]).await?;
    let raw: Vec<RawAddrInterface> = serde_json::from_str(&json)
        .map_err(|e| Error::Parse(format!("ip -j addr show: {e}; got {json:?}")))?;
    let mut out = Vec::new();
    for iface in raw {
        for a in iface.addr_info.unwrap_or_default() {
            out.push(Address {
                interface: iface.ifname.clone(),
                family: a.family,
                address: a.local,
                prefix_len: a.prefixlen,
                scope: a.scope.unwrap_or_default(),
            });
        }
    }
    Ok(out)
}

/// Lists every entry in the main routing table.
///
/// # Errors
///
/// As for [`links`].
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::iproute2;
///
/// let default = iproute2::routes(&mut shell).await?
///     .into_iter()
///     .find(|r| r.destination == "default");
/// ```
pub async fn routes(shell: &mut dyn LinuxShell) -> Result<Vec<Route>> {
    let json = run_ip_json(shell, &["-j", "route", "show"]).await?;
    let raw: Vec<RawRoute> = serde_json::from_str(&json)
        .map_err(|e| Error::Parse(format!("ip -j route show: {e}; got {json:?}")))?;
    Ok(raw.into_iter().map(Route::from).collect())
}

async fn run_ip_json(shell: &mut dyn LinuxShell, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("ip");
    for a in args {
        cmd = cmd.arg(*a);
    }
    let r = shell.run(&cmd).await?;
    Ok(r.stdout().unwrap_or("").to_string())
}

// ---------- internal JSON shapes ----------
//
// These mirror the `ip -j` output schema. They're not exposed because
// the schema is iproute2-version-specific and contains fields we
// neither parse nor have a good interpretation for.

#[derive(Deserialize)]
struct RawLink {
    ifindex: u32,
    ifname: String,
    operstate: String,
    mtu: u32,
    address: Option<String>,
}

impl From<RawLink> for Link {
    fn from(r: RawLink) -> Self {
        Self {
            index: r.ifindex,
            name: r.ifname,
            operstate: r.operstate,
            mtu: r.mtu,
            mac: r.address,
        }
    }
}

#[derive(Deserialize)]
struct RawAddrInterface {
    ifname: String,
    addr_info: Option<Vec<RawAddr>>,
}

#[derive(Deserialize)]
struct RawAddr {
    family: String,
    local: String,
    prefixlen: u8,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct RawRoute {
    dst: String,
    gateway: Option<String>,
    dev: String,
    metric: Option<u32>,
    scope: Option<String>,
    protocol: Option<String>,
}

impl From<RawRoute> for Route {
    fn from(r: RawRoute) -> Self {
        Self {
            destination: r.dst,
            gateway: r.gateway,
            interface: r.dev,
            metric: r.metric,
            scope: r.scope,
            protocol: r.protocol,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Golden samples lifted from real `ip -j` output (lightly edited
    // for line breaks). All three commands are tested against the
    // parser without going through the shell, so we catch schema
    // drift independent of any device.

    const LINK_JSON: &str = r#"[
      {"ifindex":1,"ifname":"lo","flags":["LOOPBACK","UP","LOWER_UP"],
       "mtu":65536,"qdisc":"noqueue","operstate":"UNKNOWN",
       "link_type":"loopback","address":"00:00:00:00:00:00",
       "broadcast":"00:00:00:00:00:00"},
      {"ifindex":2,"ifname":"eth0","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],
       "mtu":1500,"qdisc":"fq_codel","operstate":"UP",
       "link_type":"ether","address":"aa:bb:cc:dd:ee:ff",
       "broadcast":"ff:ff:ff:ff:ff:ff"},
      {"ifindex":3,"ifname":"wlan0","flags":["BROADCAST","MULTICAST"],
       "mtu":1500,"qdisc":"noqueue","operstate":"DOWN",
       "link_type":"ether","address":"11:22:33:44:55:66",
       "broadcast":"ff:ff:ff:ff:ff:ff"}
    ]"#;

    const ADDR_JSON: &str = r#"[
      {"ifindex":1,"ifname":"lo","operstate":"UNKNOWN",
       "addr_info":[
         {"family":"inet","local":"127.0.0.1","prefixlen":8,"scope":"host","label":"lo"},
         {"family":"inet6","local":"::1","prefixlen":128,"scope":"host"}
       ]},
      {"ifindex":2,"ifname":"eth0","operstate":"UP",
       "addr_info":[
         {"family":"inet","local":"192.168.1.5","prefixlen":24,"scope":"global","label":"eth0"},
         {"family":"inet6","local":"fe80::1","prefixlen":64,"scope":"link"}
       ]},
      {"ifindex":3,"ifname":"wlan0","operstate":"DOWN"}
    ]"#;

    const ROUTE_JSON: &str = r#"[
      {"dst":"default","gateway":"192.168.1.1","dev":"eth0",
       "protocol":"dhcp","metric":100,"flags":[]},
      {"dst":"192.168.1.0/24","dev":"eth0","protocol":"kernel",
       "scope":"link","prefsrc":"192.168.1.5","flags":[]}
    ]"#;

    #[test]
    fn parses_link_output() {
        let raw: Vec<RawLink> = serde_json::from_str(LINK_JSON).unwrap();
        let links: Vec<Link> = raw.into_iter().map(Link::from).collect();
        assert_eq!(links.len(), 3);

        assert_eq!(links[0].name, "lo");
        assert_eq!(links[0].operstate, "UNKNOWN");
        assert!(
            !links[0].is_up(),
            "loopback reports UNKNOWN, not strictly UP"
        );

        assert_eq!(links[1].name, "eth0");
        assert!(links[1].is_up());
        assert_eq!(links[1].mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        assert_eq!(links[1].mtu, 1500);

        assert_eq!(links[2].name, "wlan0");
        assert_eq!(links[2].operstate, "DOWN");
        assert!(!links[2].is_up());
    }

    #[test]
    fn flattens_addresses_across_interfaces() {
        let raw: Vec<RawAddrInterface> = serde_json::from_str(ADDR_JSON).unwrap();
        let addrs: Vec<Address> = raw
            .into_iter()
            .flat_map(|iface| {
                let name = iface.ifname.clone();
                iface
                    .addr_info
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |a| Address {
                        interface: name.clone(),
                        family: a.family,
                        address: a.local,
                        prefix_len: a.prefixlen,
                        scope: a.scope.unwrap_or_default(),
                    })
            })
            .collect();

        // Two on lo, two on eth0, zero on wlan0 (no addr_info field).
        assert_eq!(addrs.len(), 4);
        assert!(
            addrs
                .iter()
                .any(|a| a.address == "127.0.0.1" && a.is_ipv4())
        );
        assert!(addrs.iter().any(|a| a.address == "::1" && a.is_ipv6()));
        let eth_v4 = addrs
            .iter()
            .find(|a| a.interface == "eth0" && a.is_ipv4())
            .unwrap();
        assert_eq!(eth_v4.address, "192.168.1.5");
        assert_eq!(eth_v4.prefix_len, 24);
        assert_eq!(eth_v4.scope, "global");
    }

    #[test]
    fn parses_route_output() {
        let raw: Vec<RawRoute> = serde_json::from_str(ROUTE_JSON).unwrap();
        let routes: Vec<Route> = raw.into_iter().map(Route::from).collect();
        assert_eq!(routes.len(), 2);

        let default = routes.iter().find(|r| r.destination == "default").unwrap();
        assert_eq!(default.gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(default.interface, "eth0");
        assert_eq!(default.metric, Some(100));
        assert_eq!(default.protocol.as_deref(), Some("dhcp"));

        let lan = routes
            .iter()
            .find(|r| r.destination == "192.168.1.0/24")
            .unwrap();
        assert!(lan.gateway.is_none(), "link-local route has no gateway");
        assert_eq!(lan.scope.as_deref(), Some("link"));
    }

    #[test]
    fn rejects_non_json_output() {
        // Old busybox without CONFIG_FEATURE_IP_JSON prints free-form
        // text; we should error cleanly rather than misparse.
        let result = serde_json::from_str::<Vec<RawLink>>(
            "1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN",
        );
        assert!(
            result.is_err(),
            "free-form `ip` text should not parse as JSON"
        );
    }

    fn host_has_ip_json() -> bool {
        std::process::Command::new("sh")
            .args(["-c", "ip -j link show >/dev/null 2>&1"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn links_via_subprocess_shell() {
        if !host_has_ip_json() {
            eprintln!("skipping: host doesn't have `ip` with `-j` support");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        let links = links(&mut shell).await.unwrap();
        eprintln!("[test] {} links on host", links.len());
        assert!(links.iter().any(|l| l.name == "lo"));
    }

    #[tokio::test]
    async fn addresses_via_subprocess_shell() {
        if !host_has_ip_json() {
            eprintln!("skipping: host doesn't have `ip` with `-j` support");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        let addrs = addresses(&mut shell).await.unwrap();
        eprintln!("[test] {} addresses on host", addrs.len());
        assert!(
            addrs
                .iter()
                .any(|a| a.address == "127.0.0.1" && a.interface == "lo")
        );
    }

    #[tokio::test]
    async fn routes_via_subprocess_shell() {
        if !host_has_ip_json() {
            eprintln!("skipping: host doesn't have `ip` with `-j` support");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        // We just want to confirm the call returns without error and
        // serializes cleanly — content is environment-dependent.
        let routes = routes(&mut shell).await.unwrap();
        eprintln!("[test] {} routes on host", routes.len());
    }
}
