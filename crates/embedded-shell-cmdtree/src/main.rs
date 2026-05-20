//! `etree` — interactive command-tree shell binary.
//!
//! Reference impl that mounts the bundled `demo` tree (`/info`,
//! `/network`) and drops the user into a REPL. See `lib.rs` for the
//! generic engine.

use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use embedded_shell::shell::{LinuxSerialShell, LinuxShell, Shell, SubprocessShell};
use embedded_shell_cmdtree::{Repl, demo};
use tracing_subscriber::{EnvFilter, prelude::*};

#[derive(Parser)]
#[command(
    name = "etree",
    version,
    about = "Hierarchical command-tree shell for embedded-shell devices."
)]
struct Cli {
    /// Serial port. Omit for local-host (SubprocessShell) mode —
    /// useful for trying the demo tree without hooking up a device.
    #[arg(short = 'p', long = "port", env = "ETREE_PORT")]
    port: Option<String>,

    /// Login password if the device requires one. Reads
    /// `ETREE_PASSWORD` if absent.
    #[arg(long, env = "ETREE_PASSWORD", hide_env_values = true)]
    password: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    match real_main().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("etree: {e:#}");
            ExitCode::from(1)
        }
    }
}

async fn real_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let shell = open_shell(cli.port.as_deref(), cli.password.as_deref()).await?;

    let tree = demo::demo_tree();
    Repl::new(tree, shell).run().await?;
    Ok(ExitCode::SUCCESS)
}

async fn open_shell(port: Option<&str>, password: Option<&str>) -> Result<Box<dyn LinuxShell>> {
    match port {
        Some(p) => {
            let mut builder = LinuxSerialShell::builder(p);
            if let Some(pw) = password {
                builder = builder.password(pw);
            }
            let mut shell = builder.open().await.context(format!("opening {p}"))?;
            shell.activate().await.map_err(|e| anyhow!(e))?;
            Ok(Box::new(shell))
        }
        None => Ok(Box::new(SubprocessShell::new())),
    }
}
