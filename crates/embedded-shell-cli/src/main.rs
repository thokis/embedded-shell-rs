//! `eshell` — command-line driver for `embedded-shell-rs`.
//!
//! See `eshell --help`.

mod cli;
mod commands;
mod shell;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::{EnvFilter, prelude::*};

use crate::cli::{Cli, Command};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();
    let port = cli.port.as_deref();
    let password = cli.password.as_deref();

    let result = match cli.command {
        Command::Exec(args) => commands::exec::run(args, port, password).await,
        Command::Push(args) => commands::push::run(args, port, password).await,
        Command::Pull(args) => commands::pull::run(args, port, password).await,
        Command::Info(args) => commands::info::run(args, port, password).await,
        Command::Ping(args) => commands::ping::run(args, port, password).await,
        Command::Reboot(args) => commands::reboot::run(args, port, password).await,
        Command::Service(args) => commands::service::run(args, port, password).await,
        Command::Services(args) => commands::services::run(args, port, password).await,
        Command::Journal(args) => commands::journal::run(args, port, password).await,
        Command::Modem(args) => commands::modem::run(args, port, password).await,
        Command::Network(args) => commands::network::run(args, port, password).await,
        Command::Repl(args) => commands::repl::run(args, port, password).await,
        Command::Completions(args) => commands::completions::run(args),
        Command::Devices(args) => commands::devices::run(args),
        Command::Cat(args) => commands::cat::run(args, port, password).await,
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("eshell: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
