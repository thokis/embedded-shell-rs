//! Service inventory: list every active systemd service, then tail
//! the last few log entries from each that's failed.
//!
//! Run with:
//! ```sh
//! cargo run --example services --features systemd -- /dev/ttyUSB0
//! RUST_LOG=info cargo run --example services --features systemd -- /dev/ttyUSB0
//! ```
//!
//! Demonstrates [`systemd::list_units`] alongside
//! [`journalctl::tail_unit`] for a real ops workflow: "what's running
//! on this device, and what did the broken ones complain about?"

use std::time::Duration;

use embedded_shell::shell::{LinuxSerialShell, Shell};
use embedded_shell_linux::{journalctl, systemd};
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

    let services = systemd::list_units(&mut shell, Some("*.service")).await?;
    let active: Vec<_> = services.iter().filter(|u| u.is_active()).collect();
    let failed: Vec<_> = services.iter().filter(|u| u.active == "failed").collect();

    println!("==== Service inventory ({port}) ====");
    println!("Active services:  {}", active.len());
    println!("Failed services:  {}", failed.len());

    if !active.is_empty() {
        println!("\nActive:");
        for u in &active {
            println!("  {:32} {} ({})", u.unit, u.sub, u.description);
        }
    }

    if !failed.is_empty() {
        println!("\nFailed — recent log lines:");
        for u in &failed {
            println!("\n  {} — {}", u.unit, u.description);
            let entries = journalctl::tail_unit(&mut shell, &u.unit, 3).await?;
            if entries.is_empty() {
                println!("    (no log entries)");
            }
            for e in entries {
                println!(
                    "    {:?} {:?}: {}",
                    e.timestamp,
                    e.priority,
                    e.message.trim_end()
                );
            }
        }
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
