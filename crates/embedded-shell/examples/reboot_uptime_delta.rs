//! Reads `/proc/uptime`, reboots the device, waits for it to come back,
//! and reads `/proc/uptime` again — proving the reboot actually happened
//! because the new uptime is lower than the old one.
//!
//! Run with:
//! ```sh
//! cargo run --example reboot_uptime_delta -- /dev/ttyUSB0
//! RUST_LOG=embedded_shell=info cargo run --example reboot_uptime_delta -- /dev/ttyUSB0
//! ```
//!
//! Demonstrates the disconnect-reconnect lifecycle that's the whole
//! reason this library exists. After `reboot()` returns, the shell is
//! re-activated and ready for new commands — no manual reconnect needed.

use std::time::Duration;

use embedded_shell::shell::{Command, LinuxSerialShell, Shell};
use tracing::info;
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

    let before = read_uptime_seconds(&mut shell).await?;
    info!(uptime_secs = before, "uptime before reboot");

    shell.reboot().await?;
    info!("shell re-activated after reboot");

    let after = read_uptime_seconds(&mut shell).await?;
    info!(uptime_secs = after, prior = before, "uptime after reboot",);

    if after < before {
        println!("✓ reboot confirmed: uptime dropped from {before:.1}s to {after:.1}s");
    } else {
        println!("✗ unexpected: uptime did not drop ({before:.1}s → {after:.1}s)");
    }

    shell.deactivate().await?;
    Ok(())
}

async fn read_uptime_seconds(
    shell: &mut LinuxSerialShell,
) -> Result<f64, Box<dyn std::error::Error>> {
    let r = shell.run(&Command::new("cat").arg("/proc/uptime")).await?;
    let s = r.stdout().ok_or("no uptime output")?;
    let first = s
        .split_whitespace()
        .next()
        .ok_or("malformed /proc/uptime")?;
    Ok(first.parse()?)
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
