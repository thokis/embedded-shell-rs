//! `eshell services` — list systemd units grouped by active state.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::Result;
use embedded_shell_linux::systemd::{self, UnitListEntry};
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

pub async fn run(
    args: ServicesArgs,
    port: Option<&str>,
    password: Option<&str>,
) -> Result<ExitCode> {
    let mut shell = open_linux(port, password).await?;

    // Default to service-class units only — the command is `services`,
    // and listing every device/slice/scope unit by default drowns the
    // user in noise. Pass `--pattern '*'` for the full population.
    let pattern = args.pattern.as_deref().unwrap_or("*.service");
    let mut units = systemd::list_units(&mut *shell, Some(pattern)).await?;
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
    } else {
        let use_color = std::io::stdout().is_terminal();
        render_pretty(&units, use_color);
    }

    Ok(ExitCode::SUCCESS)
}

fn render_pretty(units: &[UnitListEntry], use_color: bool) {
    if units.is_empty() {
        println!("(no units matched)");
        return;
    }

    // Group by active state. BTreeMap so iteration is deterministic
    // for any state we don't have in the priority order below.
    let mut groups: BTreeMap<&str, Vec<&UnitListEntry>> = BTreeMap::new();
    for u in units {
        groups.entry(u.active.as_str()).or_default().push(u);
    }

    // Print high-signal states first; everything else (eg.
    // `deactivating`, vendor-specific states) comes after in
    // alphabetical order.
    const PRIORITY_ORDER: &[&str] = &["failed", "activating", "reloading", "active", "inactive"];
    let mut printed_states: Vec<&str> = Vec::new();

    println!();
    for state in PRIORITY_ORDER {
        if let Some(items) = groups.get(state) {
            print_section(state, items, use_color);
            printed_states.push(state);
        }
    }
    for (state, items) in &groups {
        if !printed_states.contains(state) {
            print_section(state, items, use_color);
        }
    }
}

fn print_section(state: &str, items: &[&UnitListEntry], use_color: bool) {
    let header = format!("{} ({})", title_case(state), items.len());
    println!("{}", bold(&header, use_color));

    let unit_w = items.iter().map(|u| u.unit.len()).max().unwrap_or(0);
    let (glyph, color) = state_glyph(state);
    let glyph_rendered = if use_color {
        format!("\x1b[{color}m{glyph}\x1b[0m")
    } else {
        glyph.to_string()
    };
    for u in items {
        println!(
            "  {glyph_rendered}  {unit:<unit_w$}  {desc}",
            unit = u.unit,
            desc = u.description,
        );
    }
    println!();
}

fn state_glyph(state: &str) -> (&'static str, &'static str) {
    match state {
        "failed" => ("✗", "31"),                    // red
        "active" => ("✓", "32"),                    // green
        "activating" | "reloading" => ("⟳", "33"),  // yellow
        "inactive" | "deactivating" => ("○", "90"), // dim
        _ => ("·", "37"),                           // light gray
    }
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
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
    fn title_case_capitalizes_first_letter() {
        assert_eq!(title_case("failed"), "Failed");
        assert_eq!(title_case("active"), "Active");
        assert_eq!(title_case(""), "");
        assert_eq!(title_case("X"), "X");
    }

    #[test]
    fn state_glyph_known_states() {
        assert_eq!(state_glyph("failed").0, "✗");
        assert_eq!(state_glyph("active").0, "✓");
        assert_eq!(state_glyph("activating").0, "⟳");
        assert_eq!(state_glyph("reloading").0, "⟳");
        assert_eq!(state_glyph("inactive").0, "○");
        assert_eq!(state_glyph("anything-else").0, "·");
    }
}
