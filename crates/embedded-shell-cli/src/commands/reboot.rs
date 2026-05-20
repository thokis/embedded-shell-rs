//! `eshell reboot PORT`

use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Result, anyhow};
use embedded_shell::shell::Shell;

use crate::cli::RebootArgs;
use crate::shell::open_serial;

pub async fn run(
    _args: RebootArgs,
    port: Option<&str>,
    password: Option<&str>,
) -> Result<ExitCode> {
    let port = port.ok_or_else(|| {
        anyhow!("reboot requires an explicit serial port (refusing to reboot the local host)")
    })?;
    let mut shell = open_serial(port, password).await?;

    let started = Instant::now();
    shell.reboot().await?;
    let elapsed = started.elapsed();

    let _ = shell.deactivate().await;

    println!(
        "✓ device on {} rebooted; shell re-activated in {:.1}s",
        port,
        elapsed.as_secs_f64()
    );
    Ok(ExitCode::SUCCESS)
}
