//! U-Boot shell over a serial line.
//!
//! Drives a device-side U-Boot bootloader prompt from a host-side
//! serial port.
//!
//! # Activate state machine
//!
//! [`Shell::activate`][crate::shell::Shell::activate] watches the wire
//! for one of three states:
//!
//! 1. **U-Boot autoboot banner** (`Hit any key to stop autoboot:`):
//!    sends a single newline to interrupt the countdown and waits for
//!    the prompt.
//! 2. **U-Boot prompt** (`=>`, configurable): activated, ready to run
//!    commands.
//! 3. **Linux** (login prompt or shell prompt): bails out with an
//!    [`ShellError::Initialization`] — there's no clean way to get
//!    from Linux back to U-Boot without a hardware reset.
//!
//! # Exec framing
//!
//! Commands are wrapped as `<cmd>; echo RETURNCODE=$?` so the host can
//! reliably parse the exit code from a line-anchored `^RETURNCODE=<n>$`
//! match. The framing doesn't use `\x1f` sentinels (as
//! [`LinuxSerialShell`] does) because U-Boot's `echo` doesn't
//! interpret arbitrary byte escapes.
//!
//! # No device-side timeout
//!
//! Unlike [`LinuxSerialShell`], U-Boot ships no `timeout(1)` binary —
//! command timeouts are **host-side only**. If a U-Boot command hangs
//! on the device, the host gives up after the configured timeout but
//! the device-side command keeps running until a power-cycle or hard
//! reset.
//!
//! [`LinuxSerialShell`]: super::LinuxSerialShell

use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use regex::bytes::Regex;
use tokio::time::Instant;
use tracing::{debug, info, trace};

use super::command::Command;
use super::error::ShellError;
use super::prompts::{self, PromptDetector};
use super::result::ShellResult;
use super::serial::{DEFAULT_CONSOLE_BUFFER_CAP, SerialTransport};
use super::traits::Shell;

const DEFAULT_BAUDRATE: u32 = 115_200;
const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(120);
const PROMPT_POLL: Duration = Duration::from_millis(500);
const ACTIVATE_BUFFER_CAP: usize = 8192;

/// Fluent configuration for a [`UBootSerialShell`].
///
/// Construct via [`UBootSerialShell::builder`]; finalise with
/// [`open`][Self::open]. Setters are infallible — the `shell_prompt`
/// regex is validated when [`open`][Self::open] runs, and a bad pattern
/// surfaces as [`ShellError::InvalidRegex`].
///
/// # Defaults
///
/// | Field | Default |
/// |---|---|
/// | `baudrate` | `115200` |
/// | `login_timeout` | `120s` |
/// | `console_buffer_cap` | `1 MiB` |
/// | `shell_prompt` | `=>` regex |
///
/// Note that there's no `username` / `password` field — U-Boot has no
/// login concept.
pub struct UBootSerialShellBuilder {
    port: String,
    baudrate: u32,
    login_timeout: Duration,
    console_buffer_cap: usize,
    shell_prompt: Option<String>,
}

impl UBootSerialShellBuilder {
    /// Override the baudrate. Default: `115200`.
    pub fn baudrate(mut self, baudrate: u32) -> Self {
        self.baudrate = baudrate;
        self
    }

    /// Override the login timeout. Default: `120s`.
    ///
    /// Bounds how long [`Shell::activate`][crate::shell::Shell::activate]
    /// will wait to detect a U-Boot prompt or the autoboot banner.
    pub fn login_timeout(mut self, timeout: Duration) -> Self {
        self.login_timeout = timeout;
        self
    }

    /// Cap the persistent console transcript at `cap` bytes.
    ///
    /// The reader task FIFO-trims older bytes once the captured buffer
    /// grows past `cap`. Default: 1 MiB.
    pub fn console_buffer_cap(mut self, cap: usize) -> Self {
        self.console_buffer_cap = cap;
        self
    }

    /// Override the U-Boot shell-prompt regex.
    ///
    /// Default matches `=>`. Override when your device's U-Boot uses a
    /// custom prompt (`U-Boot> `, vendor-prefixed, …). Validated at
    /// [`open`][Self::open] time; a malformed regex surfaces as
    /// [`ShellError::InvalidRegex`].
    pub fn shell_prompt(mut self, pattern: impl Into<String>) -> Self {
        self.shell_prompt = Some(pattern.into());
        self
    }

    /// Open the serial port and return an unactivated [`UBootSerialShell`].
    ///
    /// The returned shell has its transport up and reader task running,
    /// but the activate state machine has *not* yet run — call
    /// [`Shell::activate`][crate::shell::Shell::activate] to detect the
    /// U-Boot prompt before the first [`Shell::run`][crate::shell::Shell::run].
    ///
    /// # Errors
    ///
    /// - [`ShellError::InvalidRegex`] if `shell_prompt` is set to a
    ///   malformed pattern.
    /// - [`ShellError::Io`] if the serial port can't be opened.
    pub async fn open(self) -> Result<UBootSerialShell, ShellError> {
        let transport = SerialTransport::open(&self.port, self.baudrate).await?;
        self.finish(transport)
    }

    #[cfg(test)]
    pub(crate) fn build_with_transport(
        self,
        transport: SerialTransport,
    ) -> Result<UBootSerialShell, ShellError> {
        self.finish(transport)
    }

    fn finish(self, transport: SerialTransport) -> Result<UBootSerialShell, ShellError> {
        let shell_prompt = match self.shell_prompt {
            Some(p) => PromptDetector::try_compile(&p)?,
            None => PromptDetector::Default(prompts::find_uboot_shell),
        };
        transport.set_console_buffer_cap(self.console_buffer_cap);
        Ok(UBootSerialShell {
            transport,
            port: self.port,
            baudrate: self.baudrate,
            login_timeout: self.login_timeout,
            shell_detected: false,
            shell_prompt,
            console_buffer_cap: self.console_buffer_cap,
        })
    }
}

/// [`Shell`] implementation that drives a U-Boot bootloader prompt
/// over a serial line.
///
/// Construction is exclusively through [`UBootSerialShell::builder`].
/// After [`Shell::activate`] succeeds, the shell is at the U-Boot
/// prompt and ready for commands like `version`, `printenv`,
/// `bootcmd`, …
///
/// # Example
///
/// ```ignore
/// use embedded_shell::shell::{Command, Shell, UBootSerialShell};
///
/// let mut shell = UBootSerialShell::builder("/dev/ttyUSB0").open().await?;
/// shell.activate().await?;
///
/// let r = shell.run(&Command::new("version")).await?;
/// println!("{}", r.stdout().unwrap_or(""));
///
/// // Boot into Linux when done with U-Boot:
/// shell.boot_linux().await?;
/// ```
///
/// [`Shell`]: crate::shell::Shell
/// [`Shell::activate`]: crate::shell::Shell::activate
pub struct UBootSerialShell {
    transport: SerialTransport,
    port: String,
    baudrate: u32,
    login_timeout: Duration,
    shell_detected: bool,
    shell_prompt: PromptDetector,
    console_buffer_cap: usize,
}

impl UBootSerialShell {
    /// Start a fluent builder for a U-Boot shell on the given serial
    /// port.
    ///
    /// This is the only public construction path; configure via the
    /// returned [`UBootSerialShellBuilder`], then call
    /// [`UBootSerialShellBuilder::open`].
    pub fn builder(port: impl Into<String>) -> UBootSerialShellBuilder {
        UBootSerialShellBuilder {
            port: port.into(),
            baudrate: DEFAULT_BAUDRATE,
            login_timeout: DEFAULT_LOGIN_TIMEOUT,
            console_buffer_cap: DEFAULT_CONSOLE_BUFFER_CAP,
            shell_prompt: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_transport(transport: SerialTransport) -> Self {
        Self {
            transport,
            port: String::new(),
            baudrate: 0,
            login_timeout: DEFAULT_LOGIN_TIMEOUT,
            shell_detected: false,
            shell_prompt: PromptDetector::Default(prompts::find_uboot_shell),
            console_buffer_cap: DEFAULT_CONSOLE_BUFFER_CAP,
        }
    }

    /// Re-establish a working U-Boot shell after a transport
    /// disconnect.
    ///
    /// Closes the dead port, opens a fresh one with the same
    /// configuration, then runs the activate state machine again
    /// (which interrupts the autoboot banner if present). After a
    /// successful return, the shell is ready to accept commands — no
    /// separate `activate()` call needed.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Initialization`] when called on a shell built
    ///   via the test-only `from_transport` constructor (no port path
    ///   to reopen).
    /// - [`ShellError::Io`] if the port can't be reopened.
    /// - Whatever [`Shell::activate`][crate::shell::Shell::activate]
    ///   would return on the re-run.
    pub async fn reconnect(&mut self) -> Result<(), ShellError> {
        if self.port.is_empty() {
            return Err(ShellError::initialization(
                "cannot reconnect: shell was constructed without a port path \
                 (this is the case for in-memory test transports)",
            ));
        }
        info!(port = %self.port, "reconnecting u-boot serial shell");
        self.transport.close().await;
        let transport = SerialTransport::open(&self.port, self.baudrate).await?;
        transport.set_console_buffer_cap(self.console_buffer_cap);
        self.transport = transport;
        self.shell_detected = false;
        self.activate().await
    }

    /// ANSI-stripped console transcript captured since the shell was
    /// constructed.
    ///
    /// Returns everything the transport has seen on the wire, with
    /// ANSI escape sequences stripped. Particularly useful for
    /// inspecting U-Boot boot logs on activate failures.
    pub fn console_buffer(&self) -> String {
        self.transport.console_buffer()
    }

    /// Reset the device and land back at the U-Boot prompt.
    ///
    /// Sends `reset` on the wire (which triggers a hardware reset),
    /// sleeps briefly, then re-runs the activate state machine. On
    /// the way back up, the state machine catches the autoboot banner
    /// and interrupts the countdown — so the device ends at the U-Boot
    /// prompt rather than booting into Linux.
    ///
    /// Returns the total wall-clock duration on success.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Initialization`] if the post-reset activate
    ///   times out (e.g. the device didn't come back to U-Boot).
    /// - [`ShellError::Io`] on transport failure.
    pub async fn reset(&mut self) -> Result<Duration, ShellError> {
        let started = Instant::now();
        debug!("u-boot reset");
        self.transport.write_bytes(b"reset\n").await?;
        self.shell_detected = false;
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.activate().await?;
        let elapsed = started.elapsed();
        info!(?elapsed, "u-boot reset complete");
        Ok(elapsed)
    }

    /// Hand the device off to Linux and tear down the U-Boot shell.
    ///
    /// Sends `reset` and then deactivates the transport. The device
    /// reboots and the autoboot countdown runs to completion, booting
    /// into Linux. This shell instance is **not usable** afterwards —
    /// build a [`LinuxSerialShell`][super::LinuxSerialShell] (on the
    /// same port) to drive the device once Linux is up.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Io`] on transport failure while sending the
    ///   reset command.
    pub async fn boot_linux(&mut self) -> Result<(), ShellError> {
        debug!("u-boot boot into linux");
        self.transport.write_bytes(b"reset\n").await?;
        self.deactivate().await?;
        info!("u-boot handoff to linux complete");
        Ok(())
    }

    async fn exec_framed(&mut self, command: &Command) -> Result<ShellResult, ShellError> {
        let _ = self.transport.drain(Duration::from_millis(50)).await;

        let wire = command.wire_string();
        let framed = format!("{wire}; echo RETURNCODE=$?\n");
        trace!(framed = %framed.trim_end(), "sending u-boot framed command");

        let started = Utc::now();
        self.transport.write_bytes(framed.as_bytes()).await?;
        // Same idle-after-first-byte rationale as the Linux shell —
        // U-Boot's `tftp`/`loady` can stream for many seconds while
        // remaining responsive.
        let response = self
            .transport
            .read_until_progressive(
                uboot_returncode_end,
                command.timeout_duration(),
                Duration::from_secs(5),
            )
            .await?;

        let (exit_code, stdout) = parse_uboot_response(&response)?;

        let result = Box::new(ShellResult::new(
            wire.clone(),
            stdout,
            None,
            exit_code,
            started,
        ));

        if exit_code != 0 && !command.allows_nonzero() {
            return Err(ShellError::CommandFailed(result));
        }

        Ok(*result)
    }
}

#[async_trait]
impl Shell for UBootSerialShell {
    async fn activate(&mut self) -> Result<(), ShellError> {
        let started = Instant::now();
        debug!("probing u-boot serial shell");
        let mut accumulated: Vec<u8> = Vec::new();

        while !self.shell_detected && started.elapsed() < self.login_timeout {
            let remaining = self.login_timeout.saturating_sub(started.elapsed());
            let poll = PROMPT_POLL.min(remaining);

            match self.transport.read_chunk(poll).await {
                Ok(chunk) => {
                    accumulated.extend_from_slice(&chunk);
                    cap_buffer(&mut accumulated, ACTIVATE_BUFFER_CAP);

                    if prompts::find_uboot_login(&accumulated).is_some() {
                        debug!("autoboot banner detected, interrupting");
                        self.transport.write_bytes(b"\n").await?;
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        accumulated.clear();
                    } else if self.shell_prompt.find(&accumulated).is_some() {
                        debug!("u-boot prompt detected");
                        self.shell_detected = true;
                        break;
                    } else if prompts::find_linux_login(&accumulated).is_some() {
                        return Err(ShellError::initialization(
                            "device is at a Linux login prompt; cannot reach U-Boot from here \
                             without a hardware reset",
                        ));
                    } else if prompts::find_linux_shell(&accumulated).is_some() {
                        debug!("device is at a Linux shell, rebooting to catch U-Boot");
                        self.transport.write_bytes(b"reboot\n").await?;
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        accumulated.clear();
                    }
                }
                Err(ShellError::ReadTimeout { .. }) => {
                    self.transport.write_bytes(b"\n").await?;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(e) => return Err(e),
            }
        }

        if !self.shell_detected {
            return Err(ShellError::initialization(format!(
                "u-boot login timed out after {:?}",
                self.login_timeout
            )));
        }

        info!(port = %self.port, "u-boot serial shell activated");
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<(), ShellError> {
        if self.shell_detected {
            self.transport.close().await;
            self.shell_detected = false;
        }
        Ok(())
    }

    async fn run(&mut self, command: &Command) -> Result<ShellResult, ShellError> {
        self.exec_framed(command).await
    }
}

fn cap_buffer(buf: &mut Vec<u8>, max: usize) {
    if buf.len() > max {
        let drop = buf.len() - max;
        buf.drain(..drop);
    }
}

/// Predicate for `read_until`: returns the offset just past the line ending
/// the `RETURNCODE=<n>` marker, or `None` if not yet present.
fn uboot_returncode_end(buf: &[u8]) -> Option<usize> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?m)^RETURNCODE=-?\d+\r?\n").unwrap());
    re.find(buf).map(|m| m.end())
}

/// Parse a captured U-Boot response: extract the exit code from the
/// `RETURNCODE=<n>` line, strip the first line (echo of the framed command),
/// and treat everything in between as stdout.
fn parse_uboot_response(buf: &[u8]) -> Result<(i32, Option<String>), ShellError> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?m)^RETURNCODE=(-?\d+)\r?\n").unwrap());
    let captures = re.captures(buf).ok_or_else(|| {
        ShellError::initialization(format!(
            "missing RETURNCODE marker in u-boot response: {:?}",
            String::from_utf8_lossy(buf)
        ))
    })?;
    let exit_code: i32 = std::str::from_utf8(&captures[1])
        .unwrap_or("")
        .parse()
        .map_err(|_| {
            ShellError::initialization(format!(
                "could not parse u-boot exit code from {:?}",
                String::from_utf8_lossy(&captures[1])
            ))
        })?;

    let returncode_start = captures.get(0).unwrap().start();
    let body = &buf[..returncode_start];

    // Strip the first line (echo of the framed command). U-Boot has no
    // stty -echo equivalent, so the command we sent always comes back to us.
    let body_after_echo = match body.iter().position(|&b| b == b'\n') {
        Some(idx) => &body[idx + 1..],
        None => body,
    };

    let s = String::from_utf8_lossy(body_after_echo).into_owned();
    let stripped = s.trim_end_matches('\n').trim_end_matches('\r').to_string();
    let stdout = if stripped.is_empty() {
        None
    } else {
        Some(stripped)
    };

    Ok((exit_code, stdout))
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    // ---- parser unit tests ----

    #[test]
    fn returncode_end_finds_marker_line() {
        let buf = b"echo hi; echo RETURNCODE=$?\r\nhi\r\nRETURNCODE=0\r\n=>";
        let end = uboot_returncode_end(buf).unwrap();
        // end should be right after the \n following RETURNCODE=0
        assert_eq!(
            std::str::from_utf8(&buf[..end]).unwrap(),
            "echo hi; echo RETURNCODE=$?\r\nhi\r\nRETURNCODE=0\r\n"
        );
    }

    #[test]
    fn returncode_end_ignores_unanchored_match() {
        // "RETURNCODE=" appearing mid-line in command output should not trigger
        let buf = b"some output mentioning RETURNCODE=99 in passing\r\n=>";
        assert_eq!(uboot_returncode_end(buf), None);
    }

    #[test]
    fn returncode_end_matches_only_line_anchored() {
        let buf = b"echo prefix RETURNCODE=99\r\nRETURNCODE=0\r\n=>";
        let end = uboot_returncode_end(buf).unwrap();
        let captured = &buf[..end];
        // The line-anchored RETURNCODE=0 is what's captured, not the prefix one
        assert!(captured.ends_with(b"\r\nRETURNCODE=0\r\n"));
    }

    #[test]
    fn parse_response_basic() {
        let buf = b"version; echo RETURNCODE=$?\r\nU-Boot 2021.04\r\nRETURNCODE=0\r\n=>";
        let (exit, out) = parse_uboot_response(buf).unwrap();
        assert_eq!(exit, 0);
        assert_eq!(out, Some("U-Boot 2021.04".to_string()));
    }

    #[test]
    fn parse_response_multiline() {
        let buf = b"printenv; echo RETURNCODE=$?\r\nfoo=bar\r\nbaz=qux\r\nRETURNCODE=0\r\n=>";
        let (_, out) = parse_uboot_response(buf).unwrap();
        assert_eq!(out, Some("foo=bar\r\nbaz=qux".to_string()));
    }

    #[test]
    fn parse_response_empty_stdout() {
        let buf = b"true; echo RETURNCODE=$?\r\nRETURNCODE=0\r\n=>";
        let (exit, out) = parse_uboot_response(buf).unwrap();
        assert_eq!(exit, 0);
        assert_eq!(out, None);
    }

    #[test]
    fn parse_response_nonzero_exit() {
        let buf = b"bad; echo RETURNCODE=$?\r\nRETURNCODE=1\r\n=>";
        let (exit, _) = parse_uboot_response(buf).unwrap();
        assert_eq!(exit, 1);
    }

    #[test]
    fn parse_response_negative_exit() {
        let buf = b"err; echo RETURNCODE=$?\r\nRETURNCODE=-1\r\n=>";
        let (exit, _) = parse_uboot_response(buf).unwrap();
        assert_eq!(exit, -1);
    }

    #[test]
    fn parse_response_no_marker_errors() {
        assert!(parse_uboot_response(b"output but no marker\r\n=>").is_err());
    }

    // ---- transport-level integration tests ----

    fn pre_activated() -> (UBootSerialShell, tokio::io::DuplexStream) {
        let (host, device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = UBootSerialShell::from_transport(transport);
        shell.shell_detected = true;
        (shell, device)
    }

    fn respond_with(
        mut device: tokio::io::DuplexStream,
        response: Vec<u8>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut sink = vec![0u8; 4096];
            let _ = tokio::time::timeout(Duration::from_millis(200), device.read(&mut sink)).await;
            device.write_all(&response).await.unwrap();
            device.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
    }

    #[tokio::test]
    async fn run_basic_command() {
        let (mut shell, device) = pre_activated();
        let device_task = respond_with(
            device,
            b"version; echo RETURNCODE=$?\r\nU-Boot 2024.04\r\nRETURNCODE=0\r\n=>".to_vec(),
        );
        let r = shell.run(&Command::new("version")).await.unwrap();
        assert_eq!(r.exit_code(), 0);
        assert_eq!(r.stdout(), Some("U-Boot 2024.04"));
        assert!(r.stderr().is_none()); // u-boot never has stderr
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn run_command_with_nonzero_exit_returns_command_failed() {
        let (mut shell, device) = pre_activated();
        let device_task = respond_with(
            device,
            b"bogus_cmd; echo RETURNCODE=$?\r\nUnknown command 'bogus_cmd'\r\nRETURNCODE=1\r\n=>"
                .to_vec(),
        );
        let err = shell.run(&Command::new("bogus_cmd")).await.unwrap_err();
        match err {
            ShellError::CommandFailed(r) => {
                assert_eq!(r.exit_code(), 1);
                assert_eq!(r.stdout(), Some("Unknown command 'bogus_cmd'"));
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn run_command_with_allow_nonzero_returns_ok() {
        let (mut shell, device) = pre_activated();
        let device_task = respond_with(
            device,
            b"bogus; echo RETURNCODE=$?\r\nRETURNCODE=1\r\n=>".to_vec(),
        );
        let r = shell
            .run(&Command::new("bogus").allow_nonzero())
            .await
            .unwrap();
        assert_eq!(r.exit_code(), 1);
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn activate_detects_uboot_prompt_directly() {
        let (host, mut device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = UBootSerialShell::from_transport(transport);

        device.write_all(b"\r\n=> ").await.unwrap();
        device.flush().await.unwrap();

        // Keep device drained so any post-activate write doesn't block.
        let drain_task = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                if tokio::time::timeout(Duration::from_millis(500), device.read(&mut buf))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        shell.activate().await.unwrap();
        assert!(shell.shell_detected);
        drain_task.abort();
    }

    #[tokio::test]
    async fn activate_interrupts_autoboot_banner() {
        let (host, mut device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = UBootSerialShell::from_transport(transport);

        let device_task = tokio::spawn(async move {
            // 1. Emit the autoboot banner.
            device
                .write_all(b"Hit any key to stop autoboot:  3")
                .await
                .unwrap();

            // 2. Wait for the shell to send "\n" interrupting the countdown.
            let mut buf = [0u8; 1];
            device.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"\n");

            // 3. Then drop into the u-boot prompt.
            device.write_all(b"\r\n=> ").await.unwrap();

            // 4. Drain anything else.
            let mut sink = vec![0u8; 4096];
            let _ = tokio::time::timeout(Duration::from_millis(500), device.read(&mut sink)).await;
        });

        shell.activate().await.unwrap();
        assert!(shell.shell_detected);
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn activate_rejects_linux_login_prompt() {
        let (host, mut device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = UBootSerialShell::from_transport(transport);
        shell.login_timeout = Duration::from_secs(2);

        device.write_all(b"\r\ndevice login: ").await.unwrap();
        device.flush().await.unwrap();

        let drain_task = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let _ = tokio::time::timeout(Duration::from_millis(500), device.read(&mut buf)).await;
        });

        let err = shell.activate().await.unwrap_err();
        assert!(matches!(err, ShellError::Initialization(_)));
        drain_task.abort();
    }

    #[tokio::test]
    async fn activate_times_out_with_no_prompt() {
        let (host, _device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = UBootSerialShell::from_transport(transport);
        shell.login_timeout = Duration::from_millis(300);
        let err = shell.activate().await.unwrap_err();
        assert!(matches!(err, ShellError::Initialization(_)));
    }

    #[tokio::test]
    async fn activate_detects_custom_shell_prompt() {
        let (host, mut device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = UBootSerialShell::builder("dummy")
            .shell_prompt(r"device-uboot> ")
            .build_with_transport(transport)
            .unwrap();

        device.write_all(b"\r\ndevice-uboot> ").await.unwrap();
        device.flush().await.unwrap();

        let drain_task = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let _ = tokio::time::timeout(Duration::from_millis(500), device.read(&mut buf)).await;
        });

        shell.activate().await.unwrap();
        assert!(shell.shell_detected);
        drain_task.abort();
    }

    #[tokio::test]
    async fn reconnect_errors_without_port_configured() {
        let (host, _device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = UBootSerialShell::from_transport(transport);
        let err = shell.reconnect().await.unwrap_err();
        assert!(matches!(err, ShellError::Initialization(_)));
    }

    #[tokio::test]
    async fn bad_shell_prompt_regex_surfaces_as_invalid_regex_error() {
        let (host, _device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let result = UBootSerialShell::builder("dummy")
            .shell_prompt(r"(unclosed")
            .build_with_transport(transport);
        match result {
            Err(ShellError::InvalidRegex { pattern, .. }) => {
                assert_eq!(pattern, "(unclosed");
            }
            Err(other) => panic!("expected InvalidRegex, got {other:?}"),
            Ok(_) => panic!("expected an error, builder accepted the bad regex"),
        }
    }

    #[test]
    fn builder_defaults() {
        let b = UBootSerialShell::builder("/dev/ttyUSB1");
        assert_eq!(b.port, "/dev/ttyUSB1");
        assert_eq!(b.baudrate, DEFAULT_BAUDRATE);
        assert_eq!(b.login_timeout, DEFAULT_LOGIN_TIMEOUT);
        assert_eq!(b.console_buffer_cap, DEFAULT_CONSOLE_BUFFER_CAP);
        assert!(b.shell_prompt.is_none());
    }
}
