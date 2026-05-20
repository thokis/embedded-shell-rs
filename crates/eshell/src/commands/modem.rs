//! `eshell modem PORT [INDEX] [--no-sim] [--json]`

use std::process::ExitCode;

use anyhow::Result;
use embedded_shell::shell::Shell;
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

pub async fn run(args: ModemArgs, password: Option<&str>) -> Result<ExitCode> {
    let mut shell = open_linux(&args.common.port, password).await?;

    let m = modemmanager::modem(&mut shell, args.index).await?;
    let sim = if args.no_sim {
        None
    } else {
        // SIM lookup may legitimately fail (no SIM inserted, modem
        // disabled). Don't error — just leave it out of the report.
        modemmanager::sim(&mut shell, args.index).await.ok()
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
        println!("==== Modem {} ====", report.index);
        println!(
            "Make:        {}",
            report.manufacturer.as_deref().unwrap_or("?")
        );
        println!("Model:       {}", report.model.as_deref().unwrap_or("?"));
        println!("Revision:    {}", report.revision.as_deref().unwrap_or("?"));
        println!("IMEI:        {}", report.imei.as_deref().unwrap_or("-"));
        println!("State:       {}", report.state);
        if !report.access_technologies.is_empty() {
            println!("Tech:        {}", report.access_technologies.join(", "));
        }
        if let Some(q) = report.signal_quality {
            println!("Signal:      {q}%");
        }
        if let Some(op) = &report.operator_name {
            println!(
                "Operator:    {op} ({})",
                report.operator_code.as_deref().unwrap_or("?")
            );
        }
        if let Some(port) = &report.primary_port {
            println!("Primary:     {port}");
        }
        match &report.sim {
            Some(s) => {
                println!("\nSIM {}:", s.index);
                println!("  ICCID:     {}", s.iccid.as_deref().unwrap_or("-"));
                println!("  IMSI:      {}", s.imsi.as_deref().unwrap_or("-"));
                if let Some(op) = &s.operator_name {
                    println!(
                        "  Operator:  {op} ({})",
                        s.operator_code.as_deref().unwrap_or("?")
                    );
                }
            }
            None if args.no_sim => {} // intentional
            None => println!("\nSIM:         (not available)"),
        }
    }

    Ok(ExitCode::SUCCESS)
}
