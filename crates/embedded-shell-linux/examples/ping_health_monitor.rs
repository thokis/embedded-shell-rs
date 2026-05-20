//! Pings a target from the device every 5 seconds; emits info/warn
//! events on transitions (reachable → unreachable, and the reverse).
//! Stops after 10 iterations so the example terminates cleanly.
//!
//! Run with:
//! ```sh
//! cargo run --example ping_health_monitor -- /dev/ttyUSB0 8.8.8.8
//! RUST_LOG=info cargo run --example ping_health_monitor -- /dev/ttyUSB0 8.8.8.8
//! ```
//!
//! Demonstrates [`iputils::ping`] in a real loop with structured tracing.

use std::time::Duration;

use embedded_shell::shell::{LinuxSerialShell, Shell};
use embedded_shell_linux::iputils;
use tokio::time::sleep;
use tracing::{debug, info, warn};
use tracing_subscriber::{EnvFilter, prelude::*};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let mut args = std::env::args().skip(1);
    let port = args.next().unwrap_or_else(|| "/dev/ttyUSB0".to_string());
    let target = args.next().unwrap_or_else(|| "8.8.8.8".to_string());
    let interval = Duration::from_secs(5);
    let iterations = 10;

    let mut shell = LinuxSerialShell::builder(&port)
        .login_timeout(Duration::from_secs(30))
        .open()
        .await?;
    shell.activate().await?;

    let mut prev: Option<bool> = None;
    for i in 1..=iterations {
        let stats = iputils::ping(&mut shell, &target, 2).await?;
        let now = stats.is_reachable();
        match (prev, now) {
            (None, true) => info!(
                target = %target, rtt_avg_ms = ?stats.rtt_avg_ms,
                "initial state: reachable"
            ),
            (None, false) => warn!(
                target = %target, loss_percent = stats.loss_percent,
                "initial state: unreachable"
            ),
            (Some(false), true) => info!(
                target = %target, rtt_avg_ms = ?stats.rtt_avg_ms,
                "target came back up"
            ),
            (Some(true), false) => warn!(
                target = %target, loss_percent = stats.loss_percent,
                "target went down"
            ),
            _ => debug!(
                target = %target, iter = i,
                rtt_avg_ms = ?stats.rtt_avg_ms, loss_percent = stats.loss_percent,
                "state unchanged",
            ),
        }
        prev = Some(now);
        if i < iterations {
            sleep(interval).await;
        }
    }

    shell.deactivate().await?;
    Ok(())
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
