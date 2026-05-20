//! `eshell info PORT [--json]`

use std::process::ExitCode;

use anyhow::Result;
use embedded_shell::shell::{Command, Shell};
use embedded_shell_linux::fs;
use serde::Serialize;

use crate::cli::InfoArgs;
use crate::shell::open_linux;

#[derive(Serialize)]
struct DeviceInfo {
    port: String,
    os: String,
    kernel: String,
    uptime: String,
    memory: String,
    root_fs: String,
    ipv4: String,
}

pub async fn run(args: InfoArgs, password: Option<&str>) -> Result<ExitCode> {
    let port = args.common.port.clone();
    let mut shell = open_linux(&port, password).await?;

    let os_release = fs::read_to_string(&mut shell, "/etc/os-release")
        .await
        .unwrap_or_default();
    let kernel = first_line_of(shell.run(&Command::new("uname").arg("-a")).await?.stdout());
    let uptime = first_line_of(shell.run(&Command::new("uptime")).await?.stdout());
    let memory = first_line_of(
        shell
            .run(&Command::new("sh").args(["-c", "free -h 2>/dev/null | sed -n '2p'"]))
            .await?
            .stdout(),
    );
    let root_fs = first_line_of(
        shell
            .run(&Command::new("sh").args(["-c", "df -h / | tail -1"]))
            .await?
            .stdout(),
    );
    let ipv4 = first_line_of(
        shell
            .run(&Command::new("sh").args([
                "-c",
                "ip -4 -o addr show scope global 2>/dev/null || hostname -I 2>/dev/null",
            ]))
            .await?
            .stdout(),
    );

    let info = DeviceInfo {
        port,
        os: pretty_name(&os_release),
        kernel,
        uptime,
        memory,
        root_fs,
        ipv4,
    };

    let _ = shell.deactivate().await;

    if args.json {
        serde_json::to_writer(std::io::stdout(), &info)?;
        println!();
    } else {
        println!("==== Device summary ({}) ====", info.port);
        println!("OS:       {}", info.os);
        println!("Kernel:   {}", info.kernel);
        println!("Uptime:   {}", info.uptime);
        println!("Memory:   {}", info.memory);
        println!("Root fs:  {}", info.root_fs);
        println!("IPv4:     {}", info.ipv4);
    }
    Ok(ExitCode::SUCCESS)
}

fn pretty_name(os_release: &str) -> String {
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return v.trim_matches('"').to_string();
        }
    }
    "(unknown)".to_string()
}

fn first_line_of(s: Option<&str>) -> String {
    s.unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}
