//! `eshell service PORT UNIT <action> [--json]`

use std::process::ExitCode;

use anyhow::Result;
use embedded_shell::shell::Shell;
use embedded_shell_linux::systemd;
use serde::Serialize;

use crate::cli::{ServiceAction, ServiceArgs};
use crate::shell::open_linux;

#[derive(Serialize)]
struct StatusReport<'a> {
    unit: &'a str,
    active_state: &'a str,
    sub_state: &'a str,
    load_state: &'a str,
    unit_file_state: Option<&'a str>,
    description: &'a str,
}

pub async fn run(args: ServiceArgs, password: Option<&str>) -> Result<ExitCode> {
    let mut shell = open_linux(&args.common.port, password).await?;

    match args.action {
        ServiceAction::Status => {
            let s = systemd::status(&mut shell, &args.unit).await?;
            if args.json {
                let report = StatusReport {
                    unit: &args.unit,
                    active_state: &s.active_state,
                    sub_state: &s.sub_state,
                    load_state: &s.load_state,
                    unit_file_state: s.unit_file_state.as_deref(),
                    description: &s.description,
                };
                serde_json::to_writer(std::io::stdout(), &report)?;
                println!();
            } else {
                println!("Unit:     {}", args.unit);
                println!("Active:   {} ({})", s.active_state, s.sub_state);
                println!("Load:     {}", s.load_state);
                println!(
                    "On-boot:  {}",
                    s.unit_file_state.as_deref().unwrap_or("transient")
                );
                println!("Summary:  {}", s.description);
            }
            // Mirror systemctl is-active's exit-code convention so
            // scripts can branch on inactive units.
            let _ = shell.deactivate().await;
            return Ok(if s.is_active() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            });
        }
        ServiceAction::Start => systemd::start(&mut shell, &args.unit).await?,
        ServiceAction::Stop => systemd::stop(&mut shell, &args.unit).await?,
        ServiceAction::Restart => systemd::restart(&mut shell, &args.unit).await?,
        ServiceAction::Reload => systemd::reload(&mut shell, &args.unit).await?,
        ServiceAction::Enable => systemd::enable(&mut shell, &args.unit).await?,
        ServiceAction::Disable => systemd::disable(&mut shell, &args.unit).await?,
    }

    let _ = shell.deactivate().await;
    let verb = match args.action {
        ServiceAction::Start => "started",
        ServiceAction::Stop => "stopped",
        ServiceAction::Restart => "restarted",
        ServiceAction::Reload => "reloaded",
        ServiceAction::Enable => "enabled",
        ServiceAction::Disable => "disabled",
        ServiceAction::Status => unreachable!("handled above"),
    };
    println!("✓ {} {verb}", args.unit);
    Ok(ExitCode::SUCCESS)
}
