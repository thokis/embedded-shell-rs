//! `eshell journal PORT [--unit U] [-n N] [--since EXPR] [--json]`

use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use embedded_shell_linux::journalctl;
use serde::Serialize;

use crate::cli::JournalArgs;
use crate::shell::open_linux;

#[derive(Serialize)]
struct EntryReport<'a> {
    timestamp_us: u128,
    priority: Option<u8>,
    unit: Option<&'a str>,
    identifier: Option<&'a str>,
    pid: Option<u32>,
    message: &'a str,
}

pub async fn run(
    args: JournalArgs,
    port: Option<&str>,
    password: Option<&str>,
) -> Result<ExitCode> {
    let mut shell = open_linux(port, password).await?;

    // Pick the right journalctl function by which filters were given.
    let entries = match (args.unit.as_deref(), args.since.as_deref()) {
        (Some(unit), Some(since)) => journalctl::tail_unit_since(&mut *shell, unit, since).await?,
        (Some(unit), None) => journalctl::tail_unit(&mut *shell, unit, args.count).await?,
        (None, Some(since)) => journalctl::tail_since(&mut *shell, since).await?,
        (None, None) => journalctl::tail(&mut *shell, args.count).await?,
    };

    let _ = shell.deactivate().await;

    if args.json {
        for e in &entries {
            let report = EntryReport {
                timestamp_us: e
                    .timestamp
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_micros())
                    .unwrap_or(0),
                priority: e.priority.map(|p| p.as_u8()),
                unit: e.unit.as_deref(),
                identifier: e.identifier.as_deref(),
                pid: e.pid,
                message: &e.message,
            };
            serde_json::to_writer(std::io::stdout(), &report)?;
            println!();
        }
    } else {
        let use_color = std::io::stdout().is_terminal();
        for e in &entries {
            let priority_letter = e.priority.map(priority_letter).unwrap_or('-');
            let priority_color = e.priority.map(priority_color).unwrap_or(""); // no color for missing
            let priority_tag = if use_color && !priority_color.is_empty() {
                format!("\x1b[{priority_color}m[{priority_letter}]\x1b[0m")
            } else {
                format!("[{priority_letter}]")
            };
            println!(
                "{} {} {}{}: {}",
                format_timestamp(e.timestamp),
                priority_tag,
                e.identifier.as_deref().unwrap_or("?"),
                e.pid.map(|p| format!("[{p}]")).unwrap_or_default(),
                e.message.trim_end(),
            );
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// ANSI SGR code per priority, picked so the visual loudness scales
/// with severity. Empty string = no color (info / debug stay neutral).
fn priority_color(p: journalctl::Priority) -> &'static str {
    match p {
        // Bold red for emerg/alert/crit — these stop the world.
        journalctl::Priority::Emergency
        | journalctl::Priority::Alert
        | journalctl::Priority::Critical => "1;31",
        // Plain red for error.
        journalctl::Priority::Error => "31",
        // Yellow for warning.
        journalctl::Priority::Warning => "33",
        // Bold for notice — slightly louder than info.
        journalctl::Priority::Notice => "1",
        // Info: terminal default.
        journalctl::Priority::Info => "",
        // Debug: dim.
        journalctl::Priority::Debug => "90",
    }
}

fn format_timestamp(t: SystemTime) -> String {
    // ISO-8601-ish without a date library — `%Y-%m-%dT%H:%M:%S` from
    // the chrono crate would be neater, but eshell already keeps chrono
    // off its dep list. Fall back to seconds-since-epoch.
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => format!("{:>10}.{:06}", d.as_secs(), d.subsec_micros()),
        Err(_) => "          0.000000".to_string(),
    }
}

fn priority_letter(p: journalctl::Priority) -> char {
    match p {
        journalctl::Priority::Emergency => 'M',
        journalctl::Priority::Alert => 'A',
        journalctl::Priority::Critical => 'C',
        journalctl::Priority::Error => 'E',
        journalctl::Priority::Warning => 'W',
        journalctl::Priority::Notice => 'N',
        journalctl::Priority::Info => 'I',
        journalctl::Priority::Debug => 'D',
    }
}
