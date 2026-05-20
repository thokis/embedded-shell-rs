//! `eshell modem PORT [INDEX] [--no-sim] [--json]`

use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::{Result, anyhow};

use embedded_shell_linux::modemmanager;
use serde::Serialize;

use crate::cli::ModemArgs;
use crate::shell::open_linux;

#[derive(Serialize)]
struct ModemReport {
    index: u32,
    state: String,
    manufacturer: Option<String>,
    model: Option<String>,
    revision: Option<String>,
    imei: Option<String>,
    access_technologies: Vec<String>,
    signal_quality: Option<u8>,
    operator_name: Option<String>,
    operator_code: Option<String>,
    primary_port: Option<String>,
    sim: Option<SimReport>,
}

#[derive(Serialize)]
struct SimReport {
    index: u32,
    iccid: Option<String>,
    imsi: Option<String>,
    operator_name: Option<String>,
    operator_code: Option<String>,
}

pub async fn run(args: ModemArgs, port: Option<&str>, password: Option<&str>) -> Result<ExitCode> {
    let mut shell = open_linux(port, password).await?;

    // If --modem/-m isn't given, look up modems via mmcli and pick
    // the first one. This is friendly on multi-modem devices (no
    // silent default to 0) and gives a clear error when there are
    // no modems registered.
    let index = match args.index {
        Some(i) => i,
        None => {
            let indices = modemmanager::list_modems(&mut *shell).await?;
            *indices.first().ok_or_else(|| {
                anyhow!(
                    "ModemManager reports no modems on this device; \
                     pass -m <index> if you expected one"
                )
            })?
        }
    };

    let m = modemmanager::modem(&mut *shell, index).await?;
    let sim = if args.no_sim {
        None
    } else {
        // SIM lookup may legitimately fail (no SIM inserted, modem
        // disabled). Don't error — just leave it out of the report.
        modemmanager::sim(&mut *shell, index).await.ok()
    };

    let _ = shell.deactivate().await;

    let report = ModemReport {
        index: m.index,
        state: m.state.clone(),
        manufacturer: m.manufacturer.clone(),
        model: m.model.clone(),
        revision: m.revision.clone(),
        imei: m.imei.clone(),
        access_technologies: m.access_technologies.clone(),
        signal_quality: m.signal_quality,
        operator_name: m.operator_name.clone(),
        operator_code: m.operator_code.clone(),
        primary_port: m.primary_port.clone(),
        sim: sim.as_ref().map(|s| SimReport {
            index: s.index,
            iccid: s.iccid.clone(),
            imsi: s.imsi.clone(),
            operator_name: s.operator_name.clone(),
            operator_code: s.operator_code.clone(),
        }),
    };

    if args.json {
        serde_json::to_writer(std::io::stdout(), &report)?;
        println!();
    } else {
        let use_color = std::io::stdout().is_terminal();
        render_pretty(&report, args.no_sim, use_color);
    }

    Ok(ExitCode::SUCCESS)
}

fn render_pretty(report: &ModemReport, no_sim: bool, use_color: bool) {
    // 8 chars is the longest label (`Revision`, `Operator`) — pad to
    // 11 so we get a 3-space gap between label and value, matching
    // `info`.
    const LABEL_PAD: usize = 11;

    println!();
    println!("{}", bold(&format!("Modem {}", report.index), use_color));

    row_opt("Make", report.manufacturer.as_deref(), LABEL_PAD);
    row_opt("Model", report.model.as_deref(), LABEL_PAD);
    row_opt("Revision", report.revision.as_deref(), LABEL_PAD);
    row_opt("IMEI", report.imei.as_deref(), LABEL_PAD);

    let state_value = colored_state(&report.state, use_color);
    row("State", &state_value, LABEL_PAD);

    if let Some(q) = report.signal_quality {
        row("Signal", &colored_signal(q, use_color), LABEL_PAD);
    }
    if !report.access_technologies.is_empty() {
        row("Tech", &report.access_technologies.join(", "), LABEL_PAD);
    }
    row_opt("Port", report.primary_port.as_deref(), LABEL_PAD);
    if let Some(name) = &report.operator_name {
        let value = match report.operator_code.as_deref() {
            Some(code) => format!("{name} ({code})"),
            None => name.clone(),
        };
        row("Operator", &value, LABEL_PAD);
    }

    match &report.sim {
        Some(s) => {
            println!();
            println!("{}", bold(&format!("SIM {}", s.index), use_color));
            row_opt("ICCID", s.iccid.as_deref(), LABEL_PAD);
            row_opt("IMSI", s.imsi.as_deref(), LABEL_PAD);
            if let Some(name) = &s.operator_name {
                let value = match s.operator_code.as_deref() {
                    Some(code) => format!("{name} ({code})"),
                    None => name.clone(),
                };
                row("Operator", &value, LABEL_PAD);
            }
        }
        None if no_sim => {} // intentional: user passed --no-sim
        None => {
            println!();
            println!("{}", bold("SIM", use_color));
            println!("  (not available)");
        }
    }
    println!();
}

fn row(label: &str, value: &str, pad: usize) {
    println!("  {label:<pad$}{value}");
}

fn row_opt(label: &str, value: Option<&str>, pad: usize) {
    if let Some(v) = value.filter(|s| !s.is_empty()) {
        row(label, v, pad);
    }
}

/// Map a ModemManager state name to an ANSI color. Green for
/// happy steady states, yellow for transitional ones, red for
/// failure / locked.
fn colored_state(state: &str, use_color: bool) -> String {
    if !use_color {
        return state.to_string();
    }
    let code = match state {
        "connected" | "registered" => "32", // green
        "connecting" | "searching" | "enabling" | "registering" => "33", // yellow
        "disconnecting" => "33",            // yellow
        "failed" | "locked" | "disabled" | "disconnected" => "31", // red
        _ => return state.to_string(),
    };
    format!("\x1b[{code}m{state}\x1b[0m")
}

/// Signal quality as `NN%` colored by band. Green > 75 (strong),
/// yellow 25–75 (usable), red < 25 (poor).
fn colored_signal(q: u8, use_color: bool) -> String {
    let text = format!("{q}%");
    if !use_color {
        return text;
    }
    let code = match q {
        76..=u8::MAX => "32", // green
        25..=75 => "33",      // yellow
        _ => "31",            // red (< 25%)
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

fn bold(s: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colored_state_steady_states_green() {
        assert!(colored_state("connected", true).contains("\x1b[32m"));
        assert!(colored_state("registered", true).contains("\x1b[32m"));
    }

    #[test]
    fn colored_state_transitional_yellow() {
        assert!(colored_state("searching", true).contains("\x1b[33m"));
        assert!(colored_state("connecting", true).contains("\x1b[33m"));
        assert!(colored_state("registering", true).contains("\x1b[33m"));
    }

    #[test]
    fn colored_state_failures_red() {
        assert!(colored_state("failed", true).contains("\x1b[31m"));
        assert!(colored_state("locked", true).contains("\x1b[31m"));
        assert!(colored_state("disconnected", true).contains("\x1b[31m"));
    }

    #[test]
    fn colored_state_unknown_passes_through() {
        assert_eq!(colored_state("future-state", true), "future-state");
    }

    #[test]
    fn colored_signal_bands() {
        assert!(colored_signal(90, true).contains("\x1b[32m"));
        assert!(colored_signal(50, true).contains("\x1b[33m"));
        assert!(colored_signal(10, true).contains("\x1b[31m"));
        // Boundary checks
        assert!(colored_signal(76, true).contains("\x1b[32m"));
        assert!(colored_signal(25, true).contains("\x1b[33m"));
        assert!(colored_signal(24, true).contains("\x1b[31m"));
    }

    #[test]
    fn colored_signal_no_color_path() {
        assert_eq!(colored_signal(50, false), "50%");
    }
}
