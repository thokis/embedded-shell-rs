//! Connects to a Linux device and prints a one-page summary.
//!
//! Run with:
//! ```sh
//! cargo run --example device_info -- /dev/ttyUSB0
//! RUST_LOG=embedded_shell=debug cargo run --example device_info -- /dev/ttyUSB0
//! ```
//!
//! Demonstrates the foundational pattern: `builder → open → activate → run`
//! plus reading a file with [`fs::read_to_string`].

use std::time::Duration;

use embedded_shell::shell::{Command, LinuxSerialShell, Shell};
use embedded_shell_linux::fs;
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

    let os = fs::read_to_string(&mut shell, "/etc/os-release")
        .await
        .unwrap_or_default();
    let kernel = shell.run(&Command::new("uname").arg("-a")).await?;
    let uptime = shell.run(&Command::new("uptime")).await?;
    let mem = shell
        .run(&Command::new("sh").args(["-c", "free -h 2>/dev/null | sed -n '2p'"]))
        .await?;
    let df = shell
        .run(&Command::new("sh").args(["-c", "df -h / | tail -1"]))
        .await?;
    let ip = shell
        .run(&Command::new("sh").args([
            "-c",
            "ip -4 -o addr show scope global 2>/dev/null || hostname -I",
        ]))
        .await?;

    println!("==== Device summary ({port}) ====");
    println!("OS:       {}", pretty_name(&os));
    println!("Kernel:   {}", trim(&kernel));
    println!("Uptime:   {}", trim(&uptime));
    println!("Memory:   {}", trim(&mem));
    println!("Root fs:  {}", trim(&df));
    println!("IPv4:     {}", trim(&ip));

    shell.deactivate().await?;
    Ok(())
}

fn pretty_name(os_release: &str) -> String {
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return v.trim_matches('"').to_string();
        }
    }
    "(unknown)".to_string()
}

fn trim(result: &embedded_shell::shell::ShellResult) -> String {
    result
        .stdout()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
