//! Comprehensive network state inspection — interfaces, addresses,
//! default route, plus NetworkManager's view of active connections.
//!
//! Run with:
//! ```sh
//! cargo run --example network --features iproute2,networkmanager -- /dev/ttyUSB0
//! ```
//!
//! Demonstrates [`iproute2`] (kernel/network-stack view) and
//! [`networkmanager`] (daemon/config view) side-by-side. They answer
//! related but different questions: iproute2 is the "what's actually
//! plumbed right now" view; nmcli is the "what's NM trying to
//! manage" view.

use std::time::Duration;

use embedded_shell::shell::{LinuxSerialShell, Shell};
use embedded_shell_linux::{iproute2, networkmanager};
use tracing_subscriber::{EnvFilter, prelude::*};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/ttyUSB0".to_string());

    let mut shell = LinuxSerialShell::builder(&port)
        .login_timeout(Duration::from_secs(30))
        .open()
        .await?;
    shell.activate().await?;

    println!("==== Network state ({port}) ====\n");

    // Kernel view: links + default route.
    let links = iproute2::links(&mut shell).await?;
    println!("Links ({} total):", links.len());
    for l in &links {
        println!(
            "  {:8} {:8} mtu={} mac={}",
            l.name,
            l.operstate,
            l.mtu,
            l.mac.as_deref().unwrap_or("-")
        );
    }

    let addrs = iproute2::addresses(&mut shell).await?;
    println!("\nAddresses:");
    for a in addrs
        .iter()
        .filter(|a| a.scope == "global" || a.scope == "host")
    {
        println!(
            "  {:8} {}/{} ({})",
            a.interface, a.address, a.prefix_len, a.scope
        );
    }

    if let Some(gw) = iproute2::default_route(&mut shell).await? {
        println!(
            "\nDefault route: via {} dev {} ({})",
            gw.gateway.as_deref().unwrap_or("?"),
            gw.interface,
            gw.protocol.as_deref().unwrap_or("?"),
        );
    } else {
        println!("\nNo default route configured.");
    }

    // NM view: connections + devices.
    println!("\nNetworkManager connections:");
    let active = networkmanager::active_connections(&mut shell).await?;
    for c in &active {
        println!(
            "  {:24} ({}) -> {}",
            c.name,
            c.kind,
            c.device.as_deref().unwrap_or("?")
        );
    }
    if active.is_empty() {
        println!("  (none active)");
    }

    shell.deactivate().await?;
    Ok(())
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
