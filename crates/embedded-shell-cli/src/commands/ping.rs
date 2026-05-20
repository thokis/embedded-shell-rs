//! `eshell ping PORT TARGET [--count N] [--json]`

use std::process::ExitCode;

use anyhow::Result;

use embedded_shell_linux::iputils;
use serde::Serialize;

use crate::cli::PingArgs;
use crate::shell::open_linux;

#[derive(Serialize)]
struct PingReport<'a> {
    target: &'a str,
    transmitted: u32,
    received: u32,
    loss_percent: f32,
    rtt_min_ms: Option<f32>,
    rtt_avg_ms: Option<f32>,
    rtt_max_ms: Option<f32>,
}

pub async fn run(args: PingArgs, port: Option<&str>, password: Option<&str>) -> Result<ExitCode> {
    let mut shell = open_linux(port, password).await?;
    let stats = iputils::ping(&mut *shell, &args.target, args.count).await?;
    let _ = shell.deactivate().await;

    if args.json {
        let report = PingReport {
            target: &args.target,
            transmitted: stats.transmitted,
            received: stats.received,
            loss_percent: stats.loss_percent,
            rtt_min_ms: stats.rtt_min_ms,
            rtt_avg_ms: stats.rtt_avg_ms,
            rtt_max_ms: stats.rtt_max_ms,
        };
        serde_json::to_writer(std::io::stdout(), &report)?;
        println!();
    } else {
        println!(
            "{} packets transmitted, {} received, {:.0}% packet loss",
            stats.transmitted, stats.received, stats.loss_percent
        );
        if let (Some(min), Some(avg), Some(max)) =
            (stats.rtt_min_ms, stats.rtt_avg_ms, stats.rtt_max_ms)
        {
            println!("rtt min/avg/max = {min:.3}/{avg:.3}/{max:.3} ms");
        }
    }

    // Exit 1 on total loss so shell scripts can branch on reachability.
    if stats.is_reachable() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}
