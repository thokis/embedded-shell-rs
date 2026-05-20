//! `eshell push PORT --src ... --dst ... [--via http|serial] [--mode 0644]`

use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use embedded_shell::shell::{Command, LinuxShell};
use embedded_shell_transfer::{TransferError, http, serial};

use crate::cli::{PushArgs, Transport};
use crate::shell::open_linux;

pub async fn run(args: PushArgs, port: Option<&str>, password: Option<&str>) -> Result<ExitCode> {
    let port = port.ok_or_else(|| {
        anyhow!("push requires an explicit serial port (refusing to overwrite local files)")
    })?;
    let bytes =
        std::fs::read(&args.src).with_context(|| format!("reading {}", args.src.display()))?;

    let mut shell = open_linux(Some(port), password).await?;

    let used = match args.via {
        Some(Transport::Http) => {
            http::push(&mut *shell, &bytes, &args.dst)
                .await
                .context("http push (--via http forced)")?;
            "http"
        }
        Some(Transport::Serial) => {
            serial::push(&mut *shell, &bytes, &args.dst)
                .await
                .context("serial push (--via serial forced)")?;
            "serial"
        }
        None => push_auto(&mut *shell, &bytes, &args.dst).await?,
    };

    if let Some(mode) = &args.mode {
        shell
            .run(&Command::new("chmod").arg(mode).arg(&args.dst))
            .await
            .with_context(|| format!("chmod {} {}", mode, args.dst))?;
    }

    let _ = shell.deactivate().await;
    println!("✓ pushed {} bytes to {} via {used}", bytes.len(), args.dst);
    Ok(ExitCode::SUCCESS)
}

/// Try HTTP first; on an HTTP-specific failure (no host IP, no
/// downloader on device, hyper error) fall back to serial. Errors
/// rooted in the device or the file itself surface immediately.
async fn push_auto(shell: &mut dyn LinuxShell, data: &[u8], remote: &str) -> Result<&'static str> {
    match http::push(shell, data, remote).await {
        Ok(()) => Ok("http"),
        Err(e) if is_http_specific(&e) => {
            tracing::warn!(error = %e, "http push failed; falling back to serial");
            match serial::push(shell, data, remote).await {
                Ok(()) => Ok("serial"),
                Err(serial_err) => {
                    bail!("both transports failed: http: {e}; serial (fallback): {serial_err}")
                }
            }
        }
        Err(e) => Err(e.into()),
    }
}

fn is_http_specific(e: &TransferError) -> bool {
    matches!(
        e,
        TransferError::NoHostIp | TransferError::NoDownloader | TransferError::Http(_)
    )
}
