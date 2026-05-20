//! `eshell pull PORT --src ... --dst ... [--via http|serial]`

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use embedded_shell::shell::LinuxShell;
use embedded_shell_transfer::{TransferError, http, serial};

use crate::cli::{PullArgs, Transport};
use crate::shell::open_linux;

pub async fn run(args: PullArgs, password: Option<&str>) -> Result<ExitCode> {
    let port = args.common.port.as_deref().ok_or_else(|| {
        anyhow!("pull requires an explicit serial port (refusing to fetch from local host)")
    })?;
    let mut shell = open_linux(Some(port), password).await?;

    let (bytes, used) = match args.via {
        Some(Transport::Http) => (
            http::fetch(&mut *shell, &args.src)
                .await
                .context("http fetch (--via http forced)")?,
            "http",
        ),
        Some(Transport::Serial) => (
            serial::fetch(&mut *shell, &args.src)
                .await
                .context("serial fetch (--via serial forced)")?,
            "serial",
        ),
        None => fetch_auto(&mut *shell, &args.src).await?,
    };

    let _ = shell.deactivate().await;

    let local_path = resolve_dst(&args.dst, &args.src);
    std::fs::write(&local_path, &bytes)
        .with_context(|| format!("writing {}", local_path.display()))?;

    println!(
        "✓ pulled {} bytes from {} → {} via {used}",
        bytes.len(),
        args.src,
        local_path.display(),
    );
    Ok(ExitCode::SUCCESS)
}

async fn fetch_auto(shell: &mut dyn LinuxShell, remote: &str) -> Result<(Vec<u8>, &'static str)> {
    match http::fetch(shell, remote).await {
        Ok(bytes) => Ok((bytes, "http")),
        Err(e) if is_http_specific(&e) => {
            tracing::warn!(error = %e, "http fetch failed; falling back to serial");
            match serial::fetch(shell, remote).await {
                Ok(bytes) => Ok((bytes, "serial")),
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

/// If `dst` is an existing directory, append the basename of `src`;
/// otherwise treat `dst` as the literal destination file path.
fn resolve_dst(dst: &Path, src: &str) -> PathBuf {
    if dst.is_dir() {
        let basename = Path::new(src)
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("downloaded"));
        dst.join(basename)
    } else {
        dst.to_path_buf()
    }
}
