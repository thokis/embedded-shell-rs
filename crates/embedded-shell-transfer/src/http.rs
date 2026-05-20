//! HTTP transport — ephemeral host-side server + device-side `curl`/`wget`.
//!
//! Push and fetch via a short-lived [`hyper`] server bound to an
//! OS-chosen port on the host. The device pulls (push direction) or
//! posts (fetch direction) over plain HTTP. The server lives only for
//! the duration of one call.
//!
//! # When to use this
//!
//! Once the device has a network route back to the host. Throughput is
//! roughly **10 MB/s on 100 Mbit Ethernet** — three orders of magnitude
//! faster than the serial transport. Use [`crate::serial`] only when
//! the device has no network yet.
//!
//! # Device-side requirements
//!
//! - **Push:** `wget` *or* `curl`. Busybox `wget` is fine. The
//!   implementation tries `wget` first (universally present on
//!   busybox-based embedded Linux) and falls back to `curl` on
//!   `CommandNotFound`. Returns [`TransferError::NoDownloader`] if
//!   neither is installed.
//! - **Fetch:** `curl` only. Busybox `wget` does not support `POST`,
//!   which is needed to upload the file back to the host.
//!
//! # Host IP discovery
//!
//! The host IP advertised in the URL is whatever
//! [`local_ip_address::local_ip`] returns — typically the address of
//! the host's default-route interface. On multi-interface hosts (USB
//! Ethernet gadget alongside Wi-Fi, container with NAT, …) that may not
//! be the one the device sees. There's no API knob for overriding it;
//! the workaround is to ensure the host's default route points at the
//! device-facing network.
//!
//! # Security
//!
//! The URL contains an unguessable per-call token; an unrelated request
//! that arrives at the same port will get a 404. This is obscurity, not
//! authentication — don't expose the host on a hostile network during a
//! transfer. The server is HTTP-only (no TLS), so the payload is on the
//! wire in plaintext.

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tracing::{debug, info};

use embedded_shell::shell::{Command, LinuxShell, ShellError};

use crate::error::{Result, TransferError};

/// Maximum wall-clock time the device may spend transferring a single
/// payload. Passed to `curl -m` / `wget -T`.
const DEVICE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long [`fetch`] waits for the uploaded body to arrive on the
/// internal channel after `curl` has exited successfully. Should never
/// trigger in practice — `curl` returns only after the server has
/// finished reading the body.
const FETCH_BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// Push `data` to `remote_path` on the device over HTTP.
///
/// Starts a one-shot `hyper` server on the host, has the device pull
/// the payload via `curl` (falling back to `wget`), then verifies the
/// SHA-256 digest on the device.
///
/// # Errors
///
/// - [`TransferError::NoHostIp`] if [`local_ip_address::local_ip`]
///   cannot find a usable address.
/// - [`TransferError::Io`] if the host can't bind a TCP socket.
/// - [`TransferError::NoDownloader`] if neither `wget` nor `curl` is
///   installed on the device.
/// - [`TransferError::Shell`] if the downloader exits non-zero.
/// - [`TransferError::ChecksumMismatch`] if the device's `sha256sum`
///   doesn't match the host-computed digest.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_transfer::http;
///
/// let firmware = std::fs::read("firmware.bin")?;
/// http::push(&mut shell, &firmware, "/tmp/firmware.bin").await?;
/// ```
pub async fn push(
    shell: &mut dyn LinuxShell,
    data: &[u8],
    remote_path: impl AsRef<Path>,
) -> Result<()> {
    let host_ip = discover_host_ip()?;
    push_inner(shell, data, remote_path.as_ref(), host_ip).await
}

/// Fetch the contents of `remote_path` from the device over HTTP.
///
/// Starts a one-shot upload server on the host and has the device
/// `curl` the file body to it as a `POST`.
///
/// # Errors
///
/// - [`TransferError::NoHostIp`] if [`local_ip_address::local_ip`]
///   cannot find a usable address.
/// - [`TransferError::Io`] if the host can't bind a TCP socket.
/// - [`TransferError::Shell`] if `curl` is missing on the device, the
///   upload exits non-zero, or the file doesn't exist.
/// - [`TransferError::Http`] if the server never receives a body within
///   the internal timeout (transport failure after a clean `curl` exit
///   would be extremely unusual).
///
/// # Example
///
/// ```ignore
/// use embedded_shell_transfer::http;
///
/// let log_bytes = http::fetch(&mut shell, "/var/log/messages").await?;
/// ```
pub async fn fetch(shell: &mut dyn LinuxShell, remote_path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let host_ip = discover_host_ip()?;
    fetch_inner(shell, remote_path.as_ref(), host_ip).await
}

async fn push_inner(
    shell: &mut dyn LinuxShell,
    data: &[u8],
    remote_path: &Path,
    host_ip: IpAddr,
) -> Result<()> {
    let remote = remote_path.to_string_lossy().into_owned();
    let expected_hash = sha256_hex(data);
    let token = random_token(data.as_ptr() as usize);
    let payload: Arc<[u8]> = Arc::from(data);

    let bind_ip = bind_for(host_ip);
    let listener = TcpListener::bind(SocketAddr::new(bind_ip, 0)).await?;
    let port = listener.local_addr()?.port();
    let url = format!("http://{}:{}/{}", host_ip, port, token);

    info!(
        bytes = data.len(),
        url = %url,
        path = %remote,
        "http push starting",
    );
    let started = Instant::now();

    let server = tokio::spawn(serve_download(listener, token.clone(), payload));

    let download_result = run_downloader(shell, &url, &remote).await;
    server.abort();
    download_result?;

    let sum = shell.run(&Command::new("sha256sum").arg(&remote)).await?;
    let actual_hash = parse_sha256_output(sum.stdout().unwrap_or(""))?;
    if actual_hash != expected_hash {
        return Err(TransferError::ChecksumMismatch {
            expected: expected_hash,
            actual: actual_hash,
        });
    }

    info!(
        bytes = data.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        path = %remote,
        "http push verified",
    );

    Ok(())
}

async fn fetch_inner(
    shell: &mut dyn LinuxShell,
    remote_path: &Path,
    host_ip: IpAddr,
) -> Result<Vec<u8>> {
    let remote = remote_path.to_string_lossy().into_owned();
    let token = random_token(remote.as_ptr() as usize);

    let bind_ip = bind_for(host_ip);
    let listener = TcpListener::bind(SocketAddr::new(bind_ip, 0)).await?;
    let port = listener.local_addr()?.port();
    let url = format!("http://{}:{}/{}/upload", host_ip, port, token);

    info!(url = %url, path = %remote, "http fetch starting");
    let started = Instant::now();

    let (tx, rx) = oneshot::channel::<Vec<u8>>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));
    let server = tokio::spawn(serve_upload(listener, token.clone(), tx_slot));

    let upload_result = run_uploader(shell, &url, &remote).await;
    upload_result?;

    let body = timeout(FETCH_BODY_TIMEOUT, rx)
        .await
        .map_err(|_| {
            TransferError::Http(
                "device curl exited 0 but no body arrived on the host server".into(),
            )
        })?
        .map_err(|_| {
            TransferError::Http("upload server task ended before delivering the body".into())
        })?;

    server.abort();

    info!(
        bytes = body.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        path = %remote,
        "http fetch complete",
    );

    Ok(body)
}

async fn run_downloader(shell: &mut dyn LinuxShell, url: &str, remote: &str) -> Result<()> {
    let secs = DEVICE_TIMEOUT.as_secs();

    let wget_script = format!(
        "wget -q -T {} -O {} {}",
        secs,
        sh_single_quote(remote),
        sh_single_quote(url),
    );
    match shell
        .run(&Command::new("sh").args(["-c", &wget_script]))
        .await
    {
        Ok(_) => return Ok(()),
        Err(ShellError::CommandNotFound { .. }) => {
            debug!("wget not found on device, falling back to curl");
        }
        Err(e) => return Err(e.into()),
    }

    let curl_script = format!(
        "curl -fsSL -m {} {} -o {}",
        secs,
        sh_single_quote(url),
        sh_single_quote(remote),
    );
    match shell
        .run(&Command::new("sh").args(["-c", &curl_script]))
        .await
    {
        Ok(_) => Ok(()),
        Err(ShellError::CommandNotFound { .. }) => Err(TransferError::NoDownloader),
        Err(e) => Err(e.into()),
    }
}

async fn run_uploader(shell: &mut dyn LinuxShell, url: &str, remote: &str) -> Result<()> {
    let secs = DEVICE_TIMEOUT.as_secs();
    let script = format!(
        "curl -fsSL -m {} -X POST --data-binary @{} {}",
        secs,
        sh_single_quote(remote),
        sh_single_quote(url),
    );
    shell.run(&Command::new("sh").args(["-c", &script])).await?;
    Ok(())
}

async fn serve_download(listener: TcpListener, token: String, payload: Arc<[u8]>) {
    let expected_path = format!("/{}", token);
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let io = TokioIo::new(stream);
        let expected_path = expected_path.clone();
        let payload = payload.clone();
        let svc = service_fn(move |req: Request<Incoming>| {
            let expected_path = expected_path.clone();
            let payload = payload.clone();
            async move {
                let resp: Response<Full<Bytes>> =
                    if req.method() == Method::GET && req.uri().path() == expected_path {
                        Response::new(Full::new(Bytes::copy_from_slice(&payload)))
                    } else {
                        let mut r = Response::new(Full::new(Bytes::new()));
                        *r.status_mut() = StatusCode::NOT_FOUND;
                        r
                    };
                Ok::<_, Infallible>(resp)
            }
        });
        let _ = http1::Builder::new().serve_connection(io, svc).await;
    }
}

async fn serve_upload(
    listener: TcpListener,
    token: String,
    tx_slot: Arc<Mutex<Option<oneshot::Sender<Vec<u8>>>>>,
) {
    let expected_path = format!("/{}/upload", token);
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let io = TokioIo::new(stream);
        let expected_path = expected_path.clone();
        let tx_slot = tx_slot.clone();
        let svc = service_fn(move |req: Request<Incoming>| {
            let expected_path = expected_path.clone();
            let tx_slot = tx_slot.clone();
            async move {
                if req.method() == Method::POST && req.uri().path() == expected_path {
                    match req.collect().await {
                        Ok(collected) => {
                            let bytes = collected.to_bytes().to_vec();
                            if let Some(tx) = tx_slot.lock().unwrap().take() {
                                let _ = tx.send(bytes);
                            }
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
                        }
                        Err(_) => {
                            let mut r = Response::new(Full::new(Bytes::new()));
                            *r.status_mut() = StatusCode::BAD_REQUEST;
                            Ok(r)
                        }
                    }
                } else {
                    let mut r = Response::new(Full::new(Bytes::new()));
                    *r.status_mut() = StatusCode::NOT_FOUND;
                    Ok(r)
                }
            }
        });
        let _ = http1::Builder::new().serve_connection(io, svc).await;
    }
}

fn discover_host_ip() -> Result<IpAddr> {
    local_ip_address::local_ip().map_err(|_| TransferError::NoHostIp)
}

/// For tests on loopback, we bind to 127.0.0.1; otherwise bind to
/// 0.0.0.0 so the OS routes from any interface reaches us.
fn bind_for(host_ip: IpAddr) -> IpAddr {
    if host_ip.is_loopback() {
        host_ip
    } else {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    }
}

/// 32-char hex token from sha256(time_nanos || process_id || seed).
/// Not cryptographically secure — defense against a stray network
/// request guessing the URL during a transfer.
fn random_token(seed: usize) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mut h = Sha256::new();
    h.update(nanos.to_le_bytes());
    h.update(pid.to_le_bytes());
    h.update((seed as u128).to_le_bytes());
    let digest = h.finalize();
    let mut hex = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let digest = h.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn parse_sha256_output(stdout: &str) -> Result<String> {
    stdout
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| TransferError::Io(std::io::Error::other("sha256sum produced no output")))
}

fn sh_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use embedded_shell::shell::SubprocessShell;

    use super::*;

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "embedded-shell-transfer-http-test-{}-{}",
            std::process::id(),
            name
        ));
        p
    }

    fn host_has(bin: &str) -> bool {
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {bin}")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn have_downloader() -> bool {
        host_has("wget") || host_has("curl")
    }

    #[tokio::test]
    async fn push_writes_bytes_to_remote_path() {
        if !have_downloader() {
            eprintln!("skipping: neither wget nor curl on host");
            return;
        }
        let mut shell = SubprocessShell::new();
        let path = temp_path("push-basic");

        push_inner(&mut shell, b"hello http world", &path, LOOPBACK)
            .await
            .unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(&got, b"hello http world");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn push_then_fetch_round_trips() {
        if !have_downloader() || !host_has("curl") {
            eprintln!("skipping: fetch requires curl on host");
            return;
        }
        let mut shell = SubprocessShell::new();
        let path = temp_path("roundtrip");
        let original = b"the quick brown fox jumps over the lazy dog";

        push_inner(&mut shell, original, &path, LOOPBACK)
            .await
            .unwrap();
        let fetched = fetch_inner(&mut shell, &path, LOOPBACK).await.unwrap();
        assert_eq!(&fetched, original);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn push_preserves_all_byte_values() {
        if !have_downloader() {
            eprintln!("skipping: neither wget nor curl on host");
            return;
        }
        let mut shell = SubprocessShell::new();
        let path = temp_path("binary");
        let binary: Vec<u8> = (0..=255u8).collect();

        push_inner(&mut shell, &binary, &path, LOOPBACK)
            .await
            .unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(got, binary);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn push_carries_payloads_larger_than_serial_cap() {
        if !have_downloader() {
            eprintln!("skipping: neither wget nor curl on host");
            return;
        }
        let mut shell = SubprocessShell::new();
        let path = temp_path("big");
        // 256 KiB — past serial's 64 KiB cap, demonstrates HTTP is the
        // path for non-tiny payloads.
        let payload: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();

        push_inner(&mut shell, &payload, &path, LOOPBACK)
            .await
            .unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(got, payload);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn fetch_reads_an_existing_file() {
        if !host_has("curl") {
            eprintln!("skipping: fetch requires curl on host");
            return;
        }
        let mut shell = SubprocessShell::new();
        let path = temp_path("fetch-existing");
        std::fs::write(&path, b"file contents on the device side").unwrap();

        let got = fetch_inner(&mut shell, &path, LOOPBACK).await.unwrap();
        assert_eq!(&got, b"file contents on the device side");

        let _ = std::fs::remove_file(&path);
    }

    /// Mock [`LinuxShell`] that returns `CommandNotFound` for every
    /// command. Lets us exercise the `wget` + `curl` fallback path's
    /// terminal error without depending on host PATH manipulation.
    struct AlwaysMissingShell;

    impl LinuxShell for AlwaysMissingShell {}

    #[async_trait::async_trait]
    impl embedded_shell::shell::Shell for AlwaysMissingShell {
        async fn activate(&mut self) -> std::result::Result<(), ShellError> {
            Ok(())
        }
        async fn deactivate(&mut self) -> std::result::Result<(), ShellError> {
            Ok(())
        }
        async fn run(
            &mut self,
            command: &Command,
        ) -> std::result::Result<embedded_shell::shell::ShellResult, ShellError> {
            let result = embedded_shell::shell::ShellResult::new(
                command.wire_string(),
                None,
                Some("not found\n".to_string()),
                127,
                chrono::Utc::now(),
            );
            // run_downloader only ever calls `Command::new("sh")` so
            // hardcoding the base name keeps the mock free of crate
            // internals.
            Err(ShellError::CommandNotFound {
                command: "sh".to_string(),
                result: Box::new(result),
            })
        }
    }

    #[tokio::test]
    async fn push_returns_no_downloader_when_both_tools_missing() {
        let mut shell = AlwaysMissingShell;
        let err = push_inner(&mut shell, b"x", Path::new("/tmp/x"), LOOPBACK)
            .await
            .unwrap_err();
        assert!(matches!(err, TransferError::NoDownloader), "got {err:?}");
    }

    #[test]
    fn random_token_is_32_hex_chars() {
        let t = random_token(0);
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn random_tokens_vary_per_call() {
        let a = random_token(1);
        let b = random_token(2);
        assert_ne!(a, b);
    }
}
