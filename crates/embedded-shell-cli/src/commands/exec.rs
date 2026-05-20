//! `eshell exec PORT -- argv…`

use std::io::Write;
use std::process::ExitCode;

use anyhow::Result;
use embedded_shell::shell::{Command, ShellError};
use serde::Serialize;

use crate::cli::ExecArgs;
use crate::shell::open_linux;

#[derive(Serialize)]
struct ExecReport<'a> {
    cmd: &'a str,
    stdout: Option<&'a str>,
    stderr: Option<&'a str>,
    exit_code: i32,
    duration_ms: i64,
}

pub async fn run(args: ExecArgs, port: Option<&str>, password: Option<&str>) -> Result<ExitCode> {
    let mut shell = open_linux(port, password).await?;

    let mut iter = args.argv.into_iter();
    let head = iter
        .next()
        .expect("clap requires_at_least_one ensures argv non-empty");
    let mut cmd = Command::new(head);
    for arg in iter {
        cmd = cmd.arg(arg);
    }
    let cmd = cmd.allow_nonzero();

    let result = shell.run(&cmd).await;
    let _ = shell.deactivate().await;

    let result = match result {
        Ok(r) => r,
        Err(ShellError::CommandFailed(r))
        | Err(ShellError::CommandNotFound { result: r, .. })
        | Err(ShellError::Timeout { result: r, .. }) => *r,
        Err(e) => return Err(e.into()),
    };

    if args.json {
        let report = ExecReport {
            cmd: result.command(),
            stdout: result.stdout(),
            stderr: result.stderr(),
            exit_code: result.exit_code(),
            duration_ms: result.duration().num_milliseconds(),
        };
        serde_json::to_writer(std::io::stdout(), &report)?;
        println!();
    } else {
        if let Some(out) = result.stdout() {
            std::io::stdout().write_all(out.as_bytes())?;
        }
        if let Some(err) = result.stderr() {
            std::io::stderr().write_all(err.as_bytes())?;
        }
    }

    // Mirror the device's exit code, clamped to a byte (Unix exit
    // codes only go up to 255 anyway).
    let code = (result.exit_code() & 0xff) as u8;
    Ok(ExitCode::from(code))
}
