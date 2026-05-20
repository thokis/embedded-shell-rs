//! `eshell reboot PORT`

use std::process::ExitCode;
use std::time::Instant;

use anyhow::Result;
use embedded_shell::shell::Shell;

use crate::cli::RebootArgs;
use crate::shell::open_linux;

pub async fn run(args: RebootArgs, password: Option<&str>) -> Result<ExitCode> {
    let mut shell = open_linux(&args.common.port, password).await?;

    let started = Instant::now();
    shell.reboot().await?;
    let elapsed = started.elapsed();

    let _ = shell.deactivate().await;

    println!(
        "✓ device on {} rebooted; shell re-activated in {:.1}s",
        args.common.port,
        elapsed.as_secs_f64()
    );
    Ok(ExitCode::SUCCESS)
}
