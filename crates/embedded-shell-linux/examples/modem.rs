//! Cellular modem inventory: list every modem ModemManager knows
//! about, then dump details for each plus its primary SIM.
//!
//! Run with:
//! ```sh
//! cargo run --example modem --features modemmanager -- /dev/ttyUSB0
//! ```
//!
//! Demonstrates [`modemmanager::list_modems`], [`modemmanager::modem`],
//! and [`modemmanager::sim`]. Useful as a fleet-inventory tool — every
//! field needed to identify a SIM card (ICCID) and its subscriber
//! (IMSI) is captured.

use std::time::Duration;

use embedded_shell::shell::{LinuxSerialShell, Shell};
use embedded_shell_linux::modemmanager;
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

    let indices = modemmanager::list_modems(&mut shell).await?;
    println!("==== Modem inventory ({port}) ====");
    if indices.is_empty() {
        println!("ModemManager reports no modems.");
        shell.deactivate().await?;
        return Ok(());
    }
    println!("{} modem(s) registered.\n", indices.len());

    for idx in indices {
        let m = modemmanager::modem(&mut shell, idx).await?;
        println!("Modem {idx}");
        println!(
            "  Make:        {}",
            m.manufacturer.as_deref().unwrap_or("?")
        );
        println!("  Model:       {}", m.model.as_deref().unwrap_or("?"));
        println!("  Revision:    {}", m.revision.as_deref().unwrap_or("?"));
        println!("  IMEI:        {}", m.imei.as_deref().unwrap_or("-"));
        println!("  State:       {}", m.state);
        if !m.access_technologies.is_empty() {
            println!("  Tech:        {}", m.access_technologies.join(", "));
        }
        if let Some(q) = m.signal_quality {
            println!("  Signal:      {q}%");
        }
        if let Some(op) = &m.operator_name {
            println!(
                "  Operator:    {op} ({})",
                m.operator_code.as_deref().unwrap_or("?")
            );
        }
        if let Some(port) = &m.primary_port {
            println!("  Primary port: {port}");
        }

        // SIM is a separate lookup; may fail if the modem doesn't
        // have one inserted (mmcli reports `sim: --`).
        match modemmanager::sim(&mut shell, idx).await {
            Ok(sim) => {
                println!("  SIM:");
                println!("    ICCID:     {}", sim.iccid.as_deref().unwrap_or("-"));
                println!("    IMSI:      {}", sim.imsi.as_deref().unwrap_or("-"));
                if let Some(op) = &sim.operator_name {
                    println!(
                        "    Operator:  {op} ({})",
                        sim.operator_code.as_deref().unwrap_or("?")
                    );
                }
            }
            Err(e) => println!("  SIM:         not available ({e})"),
        }
        println!();
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
