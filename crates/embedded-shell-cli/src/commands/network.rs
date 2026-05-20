//! `eshell network PORT [--json]`

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

    println!("==== Network state ({}) ====\n", port.unwrap_or("local"));

    if report.links.is_empty() {
        println!("Kernel view (iproute2): unavailable — device's `ip` lacks JSON support.\n");
    } else {
        println!("Links ({}):", report.links.len());
        for l in &report.links {
            println!(
                "  {:<8} {:<8} mtu={} mac={}",
                l.name,
                l.operstate,
                l.mtu,
                l.mac.as_deref().unwrap_or("-")
            );
        }
    }

    let global_addrs: Vec<_> = report
        .addresses
        .iter()
        .filter(|a| a.scope == "global" || a.scope == "host")
        .collect();
    if !global_addrs.is_empty() {
        println!("\nAddresses:");
        for a in global_addrs {
            println!(
                "  {:<8} {}/{} ({} {})",
                a.interface, a.address, a.prefix_len, a.family, a.scope
            );
        }
    }

    match &report.default_route {
        Some(r) => println!(
            "\nDefault route: via {} dev {}",
            r.gateway.as_deref().unwrap_or("-"),
            r.interface,
        ),
        None => println!("\nNo default route configured."),
    }

    if !report.nm_active_connections.is_empty() {
        println!("\nNM active connections:");
        for c in &report.nm_active_connections {
            println!(
                "  {:<24} ({}) -> {}",
                c.name,
                c.kind,
                c.device.as_deref().unwrap_or("?")
            );
        }
    }

    Ok(ExitCode::SUCCESS)
}
