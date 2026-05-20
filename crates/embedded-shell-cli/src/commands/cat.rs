//! `eshell cat` — read one or more files from the device to stdout.

use std::io::Write;
use std::process::ExitCode;

use anyhow::Result;
use embedded_shell::shell::LinuxShell;
use embedded_shell_linux::fs;

use crate::cli::CatArgs;
use crate::shell::open_linux;

pub async fn run(args: CatArgs, port: Option<&str>, password: Option<&str>) -> Result<ExitCode> {
    let mut shell = open_linux(port, password).await?;

    let multi = args.paths.len() > 1;
    let mut any_error = false;

    for path in &args.paths {
        if multi {
            println!("==> {path} <==");
        }
        let r = if args.binary {
            read_binary(&mut *shell, path).await
        } else {
            read_text(&mut *shell, path).await
        };
        if let Err(e) = r {
            // Match `cat(1)`'s error stream + non-zero exit on any
            // missing file, but keep going so multi-file invocations
            // print what they can.
            eprintln!("eshell cat: {path}: {e}");
            any_error = true;
        }
    }

    let _ = shell.deactivate().await;
    Ok(if any_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

async fn read_text(shell: &mut dyn LinuxShell, path: &str) -> Result<()> {
    let content = fs::read_to_string(shell, path).await?;
    let mut out = std::io::stdout().lock();
    out.write_all(content.as_bytes())?;
    // The framing wrapper's `$(cat /tmp/out)` substitution strips
    // trailing newlines (POSIX rule), so files that ended with `\n`
    // come back here without it. Add one so terminals don't end up
    // with a `lb-gateway$` prompt-on-content-line look. Binary mode
    // doesn't have this problem — `fs::read()` uses base64 which is
    // byte-faithful.
    if !content.ends_with('\n') {
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(())
}

async fn read_binary(shell: &mut dyn LinuxShell, path: &str) -> Result<()> {
    let bytes = fs::read(shell, path).await?;
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes)?;
    out.flush()?;
    Ok(())
}
