//! `eshell completions <shell>` — emit a shell-completion script to stdout.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::CommandFactory;
use clap_complete::generate;

use crate::cli::{Cli, CompletionsArgs};

pub fn run(args: CompletionsArgs) -> Result<ExitCode> {
    let mut cmd = Cli::command();
    generate(args.shell, &mut cmd, "eshell", &mut io::stdout().lock());
    Ok(ExitCode::SUCCESS)
}
