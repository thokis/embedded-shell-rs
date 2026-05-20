//! `eshell services PORT [--pattern P] [--failed-only] [--json]`

use std::process::ExitCode;

use anyhow::Result;
use embedded_shell::shell::Shell;
use embedded_shell_linux::systemd;
use serde::Serialize;

use crate::cli::ServicesArgs;
use crate::shell::open_linux;

#[derive(Serialize)]
struct ServiceReport<'a> {
    unit: &'a str,
    load: &'a str,
    active: &'a str,
    sub: &'a str,
    description: &'a str,
}

pub async fn run(args: ServicesArgs, password: Option<&str>) -> Result<ExitCode> {
    let mut shell = open_linux(&args.common.port, password).await?;

    let pattern = args.pattern.as_deref();
    let mut units = systemd::list_units(&mut shell, pattern).await?;
    let _ = shell.deactivate().await;

    if args.failed_only {
        units.retain(|u| u.active == "failed");
    }

    if args.json {
        let report: Vec<ServiceReport> = units
            .iter()
            .map(|u| ServiceReport {
                unit: &u.unit,
                load: &u.load,
                active: &u.active,
                sub: &u.sub,
                description: &u.description,
            })
            .collect();
        serde_json::to_writer(std::io::stdout(), &report)?;
        println!();
    } else if units.is_empty() {
        println!("(no units matched)");
    } else {
        let unit_w = units.iter().map(|u| u.unit.len()).max().unwrap_or(0).max(4);
        for u in &units {
            println!(
                "{:width$}  {:<8} {:<9} {}",
                u.unit,
                u.active,
                u.sub,
                u.description,
                width = unit_w,
            );
        }
    }

    Ok(ExitCode::SUCCESS)
}
