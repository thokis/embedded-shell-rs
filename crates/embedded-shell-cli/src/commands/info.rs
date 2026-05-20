//! `eshell info PORT [--json]`

use std::process::ExitCode;

use anyhow::Result;
use embedded_shell::shell::Command;
use embedded_shell_linux::fs;
use serde::Serialize;

use crate::cli::InfoArgs;
use crate::shell::open_linux;

#[derive(Serialize)]
struct DeviceInfo {
    port: String,
    os: String,
    kernel: String,
    uptime: String,
    memory: String,
    root_fs: String,
    ipv4: String,
}

pub async fn run(args: InfoArgs, port: Option<&str>, password: Option<&str>) -> Result<ExitCode> {
    let port_label = port.unwrap_or("(local)").to_string();
    let mut shell = open_linux(port, password).await?;

    let os_release = fs::read_to_string(&mut *shell, "/etc/os-release")
        .await
        .unwrap_or_default();
    let kernel = first_line_of(shell.run(&Command::new("uname").arg("-a")).await?.stdout());
    let uptime = first_line_of(shell.run(&Command::new("uptime")).await?.stdout());
    let memory = first_line_of(
        shell
            .run(&Command::new("sh").args(["-c", "free -h 2>/dev/null | sed -n '2p'"]))
            .await?
            .stdout(),
    );
    let root_fs = first_line_of(
        shell
            .run(&Command::new("sh").args(["-c", "df -h / | tail -1"]))
            .await?
            .stdout(),
    );
    // Trim down `ip -o addr` to just `iface: addr/prefix` per line on
    // the device side so we don't have to parse all the columns. awk
    // gives portable behaviour across busybox and GNU `ip`.
    let ipv4 = format_ipv4(
        shell
            .run(&Command::new("sh").args([
                "-c",
                "ip -4 -o addr show scope global 2>/dev/null | awk '{print $2 \"=\" $4}' \
                 || hostname -I 2>/dev/null",
            ]))
            .await?
            .stdout(),
    );

    let info = DeviceInfo {
        port: port_label,
        os: pretty_name(&os_release),
        kernel,
        uptime,
        memory,
        root_fs,
        ipv4,
    };

    let _ = shell.deactivate().await;

    if args.json {
        serde_json::to_writer(std::io::stdout(), &info)?;
        println!();
    } else {
        println!("==== Device summary ({}) ====", info.port);
        println!("OS:       {}", info.os);
        println!("Kernel:   {}", info.kernel);
        println!("Uptime:   {}", info.uptime);
        println!("Memory:   {}", info.memory);
        println!("Root fs:  {}", info.root_fs);
        println!("IPv4:     {}", info.ipv4);
    }
    Ok(ExitCode::SUCCESS)
}

fn pretty_name(os_release: &str) -> String {
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return v.trim_matches('"').to_string();
        }
    }
    "(unknown)".to_string()
}

fn first_line_of(s: Option<&str>) -> String {
    s.unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Collapses the device-side `ip -o addr` output (one `iface=addr/prefix`
/// per line) into a single comma-separated string for display.
/// Falls through to the raw output if the format doesn't match the
/// expected shape (e.g. `hostname -I` fallback path on devices that
/// don't have `ip`).
fn format_ipv4(s: Option<&str>) -> String {
    let raw = s.unwrap_or("").trim();
    if raw.is_empty() {
        return "(none)".to_string();
    }
    // Lines from the awk pipeline look like `eth0=192.168.1.5/24`;
    // lines from `hostname -I` look like `192.168.1.5 10.0.0.5`.
    if raw.contains('=') {
        raw.lines()
            .map(|l| l.replace('=', ": "))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        raw.split_whitespace().collect::<Vec<_>>().join(", ")
    }
}
