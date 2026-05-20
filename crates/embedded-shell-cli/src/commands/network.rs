//! `eshell network` — kernel + NetworkManager state in one page.

use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::Result;
use embedded_shell_linux::{iproute2, networkmanager};
use serde::Serialize;

use crate::cli::NetworkArgs;
use crate::shell::open_linux;

#[derive(Serialize)]
struct NetworkReport {
    links: Vec<LinkReport>,
    addresses: Vec<AddressReport>,
    default_route: Option<RouteReport>,
    nm_active_connections: Vec<ConnectionReport>,
}

#[derive(Serialize)]
struct LinkReport {
    index: u32,
    name: String,
    operstate: String,
    mtu: u32,
    mac: Option<String>,
}

#[derive(Serialize)]
struct AddressReport {
    interface: String,
    family: String,
    address: String,
    prefix_len: u8,
    scope: String,
}

#[derive(Serialize)]
struct RouteReport {
    destination: String,
    gateway: Option<String>,
    interface: String,
    metric: Option<u32>,
}

#[derive(Serialize)]
struct ConnectionReport {
    name: String,
    kind: String,
    device: Option<String>,
}

pub async fn run(
    args: NetworkArgs,
    port: Option<&str>,
    password: Option<&str>,
) -> Result<ExitCode> {
    let mut shell = open_linux(port, password).await?;

    // iproute2 requires `ip -j` JSON support on the device; older
    // busybox builds don't have it. Treat any iproute2 failure as
    // "no kernel-view data available" rather than bailing the whole
    // command — the NM view is still useful on its own.
    let (links, addrs, default) = match iproute2::links(&mut *shell).await {
        Ok(ls) => {
            let addrs = iproute2::addresses(&mut *shell).await.unwrap_or_default();
            let default = iproute2::default_route(&mut *shell).await.unwrap_or(None);
            (ls, addrs, default)
        }
        Err(_) => (Vec::new(), Vec::new(), None),
    };
    let nm_active = networkmanager::active_connections(&mut *shell)
        .await
        .unwrap_or_default();

    let _ = shell.deactivate().await;

    let report = NetworkReport {
        links: links
            .iter()
            .map(|l| LinkReport {
                index: l.index,
                name: l.name.clone(),
                operstate: l.operstate.clone(),
                mtu: l.mtu,
                mac: l.mac.clone(),
            })
            .collect(),
        addresses: addrs
            .iter()
            .map(|a| AddressReport {
                interface: a.interface.clone(),
                family: a.family.clone(),
                address: a.address.clone(),
                prefix_len: a.prefix_len,
                scope: a.scope.clone(),
            })
            .collect(),
        default_route: default.as_ref().map(|r| RouteReport {
            destination: r.destination.clone(),
            gateway: r.gateway.clone(),
            interface: r.interface.clone(),
            metric: r.metric,
        }),
        nm_active_connections: nm_active
            .iter()
            .map(|c| ConnectionReport {
                name: c.name.clone(),
                kind: c.kind.clone(),
                device: c.device.clone(),
            })
            .collect(),
    };

    if args.json {
        serde_json::to_writer(std::io::stdout(), &report)?;
        println!();
        return Ok(ExitCode::SUCCESS);
    }

    let use_color = std::io::stdout().is_terminal();
    render_pretty(&report, use_color);
    Ok(ExitCode::SUCCESS)
}

fn render_pretty(report: &NetworkReport, use_color: bool) {
    println!();
    render_links(&report.links, use_color);
    render_addresses(&report.addresses, use_color);
    render_default_route(report.default_route.as_ref(), use_color);
    render_connections(&report.nm_active_connections, use_color);
}

fn render_links(links: &[LinkReport], use_color: bool) {
    println!("{}", bold("Links", use_color));
    if links.is_empty() {
        println!("  (kernel view unavailable — device's `ip` may lack JSON support)");
        println!();
        return;
    }
    let name_w = links.iter().map(|l| l.name.len()).max().unwrap_or(0);
    let state_w = links.iter().map(|l| l.operstate.len()).max().unwrap_or(0);
    let mtu_w = links.iter().map(|l| digits(l.mtu)).max().unwrap_or(0);
    for l in links {
        let state = colored_state(&l.operstate, use_color);
        let state_pad = " ".repeat(state_w.saturating_sub(l.operstate.len()));
        println!(
            "  {name:<name_w$}  {state}{state_pad}  mtu {mtu:<mtu_w$}   {mac}",
            name = l.name,
            mtu = l.mtu,
            mac = l.mac.as_deref().unwrap_or("-"),
        );
    }
    println!();
}

fn render_addresses(addresses: &[AddressReport], use_color: bool) {
    // Hide link-local and loopback noise by default — keep `global`
    // (the addresses you reach from off-host) and `host` (lo /
    // anything the device considers itself).
    let visible: Vec<&AddressReport> = addresses
        .iter()
        .filter(|a| a.scope == "global" || a.scope == "host")
        .collect();
    if visible.is_empty() {
        return;
    }
    println!("{}", bold("Addresses", use_color));
    let iface_w = visible.iter().map(|a| a.interface.len()).max().unwrap_or(0);
    for a in &visible {
        println!(
            "  {iface:<iface_w$}   {addr}/{prefix}",
            iface = a.interface,
            addr = a.address,
            prefix = a.prefix_len,
        );
    }
    println!();
}

fn render_default_route(route: Option<&RouteReport>, use_color: bool) {
    println!("{}", bold("Default route", use_color));
    match route {
        Some(r) => println!(
            "  via {}   dev {}",
            r.gateway.as_deref().unwrap_or("(none)"),
            r.interface,
        ),
        None => println!("  (none configured)"),
    }
    println!();
}

fn render_connections(connections: &[ConnectionReport], use_color: bool) {
    if connections.is_empty() {
        return;
    }
    println!("{}", bold("Connections", use_color));
    let name_w = connections.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let kind_w = connections.iter().map(|c| c.kind.len()).max().unwrap_or(0);
    for c in connections {
        println!(
            "  {name:<name_w$}   {kind:<kind_w$}   {device}",
            name = c.name,
            kind = c.kind,
            device = c.device.as_deref().unwrap_or("-"),
        );
    }
    println!();
}

/// Render an iface state with color when the terminal supports it.
/// Picks readable defaults: `UP` green, `DOWN` red, everything else
/// (UNKNOWN, DORMANT, …) plain.
fn colored_state(state: &str, use_color: bool) -> String {
    if !use_color {
        return state.to_string();
    }
    let code = match state {
        "UP" => "32",      // green
        "DOWN" => "31",    // red
        "UNKNOWN" => "33", // yellow
        _ => return state.to_string(),
    };
    format!("\x1b[{code}m{state}\x1b[0m")
}

fn bold(s: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn digits(n: u32) -> usize {
    if n == 0 {
        1
    } else {
        (n as f64).log10().floor() as usize + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_counts_correctly() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(1500), 4);
        assert_eq!(digits(65536), 5);
    }

    #[test]
    fn colored_state_known_codes() {
        assert!(colored_state("UP", true).contains("\x1b[32m"));
        assert!(colored_state("DOWN", true).contains("\x1b[31m"));
        assert!(colored_state("UNKNOWN", true).contains("\x1b[33m"));
        // Unknown state names pass through without ANSI codes.
        assert_eq!(colored_state("DORMANT", true), "DORMANT");
    }

    #[test]
    fn colored_state_no_color_path() {
        assert_eq!(colored_state("UP", false), "UP");
    }
}
