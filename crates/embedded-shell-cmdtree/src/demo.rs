//! Reference command tree: `/info` and `/network`.
//!
//! Bundled with the crate behind the `demo` feature so the `etree`
//! binary has something interactive to demonstrate. Downstream crates
//! disable this feature and mount their own subtrees instead.

use anyhow::Result;
use async_trait::async_trait;
use embedded_shell::shell::{Command, LinuxShell};
use embedded_shell_linux::{fs, iproute2, networkmanager};

use crate::{CommandTree, Handler, Invocation, Leaf};

/// Build the demo tree.
pub fn demo_tree() -> CommandTree {
    let mut tree = CommandTree::new();
    tree.add(
        "/info",
        "Device summary (OS, kernel, uptime, memory, disk).",
        Leaf::new(InfoHandler),
    );
    tree.add(
        "/network",
        "Network state — links, addresses, default route, NetworkManager connections.",
        Leaf::new(NetworkHandler),
    );
    tree
}

// ---------------------------------------------------------------------
// /info
// ---------------------------------------------------------------------

struct InfoHandler;

#[async_trait]
impl Handler for InfoHandler {
    async fn invoke(&self, _: &Invocation, shell: &mut dyn LinuxShell) -> Result<()> {
        let os_release = fs::read_to_string(shell, "/etc/os-release")
            .await
            .unwrap_or_default();
        let sys = shell
            .run(&Command::new("sh").args([
                "-c",
                "printf '%s\\n%s\\n%s\\n' \"$(uname -r)\" \"$(uname -m)\" \"$(hostname)\"",
            ]))
            .await?;
        let sys_lines: Vec<&str> = sys.stdout().unwrap_or("").lines().collect();
        let kernel = sys_lines.first().copied().unwrap_or("").trim();
        let arch = sys_lines.get(1).copied().unwrap_or("").trim();
        let hostname = sys_lines.get(2).copied().unwrap_or("").trim();

        let uptime = first_line(
            shell
                .run(&Command::new("uptime"))
                .await?
                .stdout()
                .unwrap_or(""),
        );

        println!();
        println!("\x1b[1m{hostname}\x1b[0m");
        println!();
        println!("  OS         {}", pretty_name(&os_release));
        println!("  Kernel     {kernel} ({arch})");
        println!("  Uptime     {}", uptime.trim());
        println!();
        Ok(())
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

fn pretty_name(os_release: &str) -> String {
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return v.trim_matches('"').to_string();
        }
    }
    "(unknown)".to_string()
}

// ---------------------------------------------------------------------
// /network
// ---------------------------------------------------------------------

struct NetworkHandler;

#[async_trait]
impl Handler for NetworkHandler {
    async fn invoke(&self, _: &Invocation, shell: &mut dyn LinuxShell) -> Result<()> {
        let links = iproute2::links(shell).await.unwrap_or_default();
        let addrs = iproute2::addresses(shell).await.unwrap_or_default();
        let default = iproute2::default_route(shell).await.unwrap_or(None);
        let nm = networkmanager::active_connections(shell)
            .await
            .unwrap_or_default();

        println!();
        println!("\x1b[1mLinks\x1b[0m");
        if links.is_empty() {
            println!("  (no kernel-view data — device's `ip` may lack JSON support)");
        } else {
            let name_w = links.iter().map(|l| l.name.len()).max().unwrap_or(0);
            let state_w = links.iter().map(|l| l.operstate.len()).max().unwrap_or(0);
            for l in &links {
                let state_color = match l.operstate.as_str() {
                    "UP" => "32",
                    "DOWN" => "31",
                    "UNKNOWN" => "33",
                    _ => "0",
                };
                println!(
                    "  {name:<name_w$}  \x1b[{state_color}m{state:<state_w$}\x1b[0m  {mac}",
                    name = l.name,
                    state = l.operstate,
                    mac = l.mac.as_deref().unwrap_or("-"),
                );
            }
        }

        let visible: Vec<_> = addrs
            .iter()
            .filter(|a| a.scope == "global" || a.scope == "host")
            .collect();
        if !visible.is_empty() {
            println!();
            println!("\x1b[1mAddresses\x1b[0m");
            let iface_w = visible.iter().map(|a| a.interface.len()).max().unwrap_or(0);
            for a in &visible {
                println!(
                    "  {iface:<iface_w$}   {addr}/{prefix}",
                    iface = a.interface,
                    addr = a.address,
                    prefix = a.prefix_len,
                );
            }
        }

        println!();
        println!("\x1b[1mDefault route\x1b[0m");
        match default {
            Some(r) => println!(
                "  via {}   dev {}",
                r.gateway.as_deref().unwrap_or("(none)"),
                r.interface
            ),
            None => println!("  (none configured)"),
        }

        if !nm.is_empty() {
            println!();
            println!("\x1b[1mConnections\x1b[0m");
            let name_w = nm.iter().map(|c| c.name.len()).max().unwrap_or(0);
            let kind_w = nm.iter().map(|c| c.kind.len()).max().unwrap_or(0);
            for c in &nm {
                println!(
                    "  {name:<name_w$}   {kind:<kind_w$}   {dev}",
                    name = c.name,
                    kind = c.kind,
                    dev = c.device.as_deref().unwrap_or("-"),
                );
            }
        }
        println!();
        Ok(())
    }
}
