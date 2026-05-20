//! Linux shell over a serial line.
//!
//! Drives a device-side Linux login + `bash`/`busybox` shell from a
//! host-side serial port.
//!
//! # Activate state machine
//!
//! [`Shell::activate`][crate::shell::Shell::activate] runs a small state
//! machine over the device's wire output:
//!
//! 1. Wait for a recognisable prompt (login prompt, shell prompt, or
//!    U-Boot prompt).
//! 2. On a login prompt: send the username, then the password if the
//!    device asks for one.
//! 3. On a U-Boot prompt: send `reset` and let the device come back up
//!    in Linux.
//! 4. Once a shell prompt is detected, configure the device terminal
//!    (`dmesg -n1`, `stty -echo`) so the framing parses cleanly.
//!
//! Custom prompt regexes are supported via
//! [`LinuxSerialShellBuilder::shell_prompt`] /
//! [`LinuxSerialShellBuilder::login_prompt`].
//!
//! # Exec framing
//!
//! After activation, every command is wrapped on the device side as:
//!
//! ```text
//! (timeout <secs> <cmd>) > /tmp/out 2> /tmp/err; \
//!     echo -e "$(echo $?)\x1f\n$(cat /tmp/out)\x1f\n$(cat /tmp/err)\x1f"
//! ```
//!
//! The host reads bytes until it has seen three `\x1f` (US) sentinel
//! bytes, then splits on `\x1f` to recover exit code, stdout, and
//! stderr. No prompt-matching is involved in capturing command output,
//! so prompt-shaped strings in the output don't confuse the parser.

use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use regex::Regex;
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};

use super::command::Command;
use super::error::ShellError;
use super::prompts::{self, PromptDetector};
use super::result::ShellResult;
use super::serial::{DEFAULT_CONSOLE_BUFFER_CAP, SerialTransport};
use super::traits::{LinuxShell, Shell};

const SENTINEL: u8 = 0x1f;
const DEFAULT_BAUDRATE: u32 = 115_200;
const DEFAULT_USERNAME: &str = "root";
const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_SILENCE_THRESHOLD: Duration = Duration::from_secs(2);
const PROMPT_POLL: Duration = Duration::from_millis(500);
const ACTIVATE_BUFFER_CAP: usize = 8192;

/// Fluent configuration for a [`LinuxSerialShell`].
///
/// Construct via [`LinuxSerialShell::builder`]; finalise with
/// [`open`][Self::open]. Setters are infallible — regex patterns are
/// validated when [`open`][Self::open] runs, and a bad pattern is
/// surfaced as [`ShellError::InvalidRegex`].
///
/// # Defaults
///
/// | Field | Default |
/// |---|---|
/// | `baudrate` | `115200` |
/// | `username` | `"root"` |
/// | `password` | none (passwordless login) |
/// | `login_timeout` | `120s` |
/// | `shutdown_timeout` | `30s` |
/// | `console_buffer_cap` | `1 MiB` |
/// | `shell_prompt` | `(root@.+:.+\#)` regex |
/// | `login_prompt` | `login: ` with `Last login:` skip |
///
/// # Example
///
/// ```ignore
/// use embedded_shell::shell::{LinuxSerialShell, Shell};
///
/// let mut shell = LinuxSerialShell::builder("/dev/ttyUSB0")
///     .username("admin")
///     .password("secret")
///     .open()
///     .await?;
/// shell.activate().await?;
/// ```
pub struct LinuxSerialShellBuilder {
    port: String,
    baudrate: u32,
    username: String,
    password: Option<String>,
    login_timeout: Duration,
    shutdown_timeout: Duration,
    console_buffer_cap: usize,
    shell_prompt: Option<String>,
    login_prompt: Option<String>,
}

impl LinuxSerialShellBuilder {
    /// Override the baudrate. Default: `115200`.
    pub fn baudrate(mut self, baudrate: u32) -> Self {
        self.baudrate = baudrate;
        self
    }

    /// Override the login username. Default: `"root"`.
    pub fn username(mut self, username: impl Into<String>) -> Self {
        self.username = username.into();
        self
    }

    /// Provide a password for the login flow.
    ///
    /// If omitted, the shell expects a passwordless login (autologin
    /// or no password configured). With a password set, the activate
    /// state machine sends it in response to a `Password:` prompt
    /// after the username.
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// Override the overall login timeout. Default: `120s`.
    ///
    /// Bounds how long [`Shell::activate`][crate::shell::Shell::activate]
    /// (and the post-reboot activate in [`LinuxSerialShell::reboot`])
    /// will wait for a recognisable prompt. Set generously when the
    /// device is slow to boot.
    pub fn login_timeout(mut self, timeout: Duration) -> Self {
        self.login_timeout = timeout;
        self
    }

    /// Override the maximum time the host waits for the device to fall
    /// silent during a shutdown. Default: `30s`.
    ///
    /// Used by both [`LinuxSerialShell::reboot`] (during its shutdown
    /// phase, before the post-reboot activate) and
    /// [`LinuxSerialShell::shutdown`] (the standalone `poweroff` flow).
    /// In both cases the host drains bytes off the wire until it has
    /// been silent for ~2 seconds (signal that userspace is gone and
    /// the kernel is mid-reset / fully off), capped at this timeout.
    ///
    /// Devices with many services to stop, large disk buffers to
    /// flush, or chatty shutdown scripts can spend minutes producing
    /// output — set this generously (e.g. `180s` or `240s`) for those
    /// cases.
    ///
    /// This is separate from [`login_timeout`][Self::login_timeout],
    /// which governs the post-reboot activate phase.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Cap the persistent console transcript at `cap` bytes.
    ///
    /// The reader task FIFO-trims older bytes once the captured buffer
    /// grows past `cap`. Default: 1 MiB — covers verbose boot logs
    /// (typically ~50 KiB) with two orders of magnitude of headroom.
    /// Reduce for memory-constrained hosts; increase only if you need
    /// extremely long-running transcripts.
    pub fn console_buffer_cap(mut self, cap: usize) -> Self {
        self.console_buffer_cap = cap;
        self
    }

    /// Override the shell-prompt regex.
    ///
    /// Default matches `root@<host>:<cwd>#` (the typical `bash`/
    /// `busybox` prompt for `root`). Override when your device uses a
    /// different format (custom PS1, non-root user, decorated prompt).
    ///
    /// Validated at [`open`][Self::open] time; a malformed regex
    /// surfaces as [`ShellError::InvalidRegex`].
    pub fn shell_prompt(mut self, pattern: impl Into<String>) -> Self {
        self.shell_prompt = Some(pattern.into());
        self
    }

    /// Override the login-prompt regex.
    ///
    /// Default matches `login: ` with a built-in exclusion for SSH-style
    /// `Last login: ...` banner lines. The exclusion is **not** applied
    /// to custom patterns — express it in your regex if your device
    /// emits SSH banners.
    ///
    /// Validated at [`open`][Self::open] time; a malformed regex
    /// surfaces as [`ShellError::InvalidRegex`].
    pub fn login_prompt(mut self, pattern: impl Into<String>) -> Self {
        self.login_prompt = Some(pattern.into());
        self
    }

    /// Open the serial port and return an unactivated [`LinuxSerialShell`].
    ///
    /// The returned shell has its transport up and reader task running,
    /// but the login state machine has *not* yet run — call
    /// [`Shell::activate`][crate::shell::Shell::activate] to log in
    /// before the first [`Shell::run`][crate::shell::Shell::run].
    ///
    /// # Errors
    ///
    /// - [`ShellError::InvalidRegex`] if a custom `shell_prompt` or
    ///   `login_prompt` pattern fails to compile.
    /// - [`ShellError::Io`] if the serial port can't be opened
    ///   (missing, busy, permission denied, …).
    pub async fn open(self) -> Result<LinuxSerialShell, ShellError> {
        let transport = SerialTransport::open(&self.port, self.baudrate).await?;
        self.finish(transport)
    }

    /// Test affordance: validate config and build using an injected transport.
    #[cfg(test)]
    pub(crate) fn build_with_transport(
        self,
        transport: SerialTransport,
    ) -> Result<LinuxSerialShell, ShellError> {
        self.finish(transport)
    }

    fn finish(self, transport: SerialTransport) -> Result<LinuxSerialShell, ShellError> {
        let shell_prompt = match self.shell_prompt {
            Some(p) => PromptDetector::try_compile(&p)?,
            None => PromptDetector::Default(prompts::find_linux_shell),
        };
        let login_prompt = match self.login_prompt {
            Some(p) => PromptDetector::try_compile(&p)?,
            None => PromptDetector::Default(prompts::find_linux_login),
        };
        transport.set_console_buffer_cap(self.console_buffer_cap);
        Ok(LinuxSerialShell {
            transport,
            port: self.port,
            baudrate: self.baudrate,
            username: self.username,
            password: self.password,
            login_timeout: self.login_timeout,
            shutdown_timeout: self.shutdown_timeout,
            shell_detected: false,
            shell_prompt,
            login_prompt,
            console_buffer_cap: self.console_buffer_cap,
        })
    }
}

/// [`Shell`] implementation that drives a Linux login + bash/busybox
/// over a serial line.
///
/// Construction is exclusively through [`LinuxSerialShell::builder`].
/// After [`Shell::activate`] succeeds, calls to [`Shell::run`] are
/// safe; before that, the shell isn't ready to accept commands.
///
/// # Example
///
/// ```ignore
/// use embedded_shell::shell::{Command, LinuxSerialShell, Shell};
///
/// let mut shell = LinuxSerialShell::builder("/dev/ttyUSB0")
///     .password("raspberry")
///     .open()
///     .await?;
/// shell.activate().await?;
///
/// let r = shell.run(&Command::new("uname").arg("-a")).await?;
/// println!("{}", r.stdout().unwrap_or(""));
///
/// shell.deactivate().await?;
/// ```
///
/// [`Shell`]: crate::shell::Shell
/// [`Shell::activate`]: crate::shell::Shell::activate
/// [`Shell::run`]: crate::shell::Shell::run
pub struct LinuxSerialShell {
    transport: SerialTransport,
    port: String,
    baudrate: u32,
    username: String,
    password: Option<String>,
    login_timeout: Duration,
    shutdown_timeout: Duration,
    shell_detected: bool,
    shell_prompt: PromptDetector,
    login_prompt: PromptDetector,
    console_buffer_cap: usize,
}

impl LinuxShell for LinuxSerialShell {}

impl LinuxSerialShell {
    /// Start a fluent builder for a shell on the given serial port.
    ///
    /// This is the only public construction path; configure via the
    /// returned [`LinuxSerialShellBuilder`], then call
    /// [`LinuxSerialShellBuilder::open`].
    ///
    /// # Example
    ///
    /// ```ignore
    /// use embedded_shell::shell::LinuxSerialShell;
    ///
    /// let mut shell = LinuxSerialShell::builder("/dev/ttyUSB0")
    ///     .open()
    ///     .await?;
    /// ```
    pub fn builder(port: impl Into<String>) -> LinuxSerialShellBuilder {
        LinuxSerialShellBuilder {
            port: port.into(),
            baudrate: DEFAULT_BAUDRATE,
            username: DEFAULT_USERNAME.to_string(),
            password: None,
            login_timeout: DEFAULT_LOGIN_TIMEOUT,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            console_buffer_cap: DEFAULT_CONSOLE_BUFFER_CAP,
            shell_prompt: None,
            login_prompt: None,
        }
    }

    /// Test-only constructor for injecting an in-memory transport. From
    /// outside the crate the only way to build a shell is via [`Self::builder`].
    #[cfg(test)]
    pub(crate) fn from_transport(transport: SerialTransport) -> Self {
        Self {
            transport,
            port: String::new(),
            baudrate: 0,
            username: DEFAULT_USERNAME.to_string(),
            password: None,
            login_timeout: DEFAULT_LOGIN_TIMEOUT,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            shell_detected: false,
            shell_prompt: PromptDetector::Default(prompts::find_linux_shell),
            login_prompt: PromptDetector::Default(prompts::find_linux_login),
            console_buffer_cap: DEFAULT_CONSOLE_BUFFER_CAP,
        }
    }

    /// Re-establish a working shell after a transport disconnect: close the
    /// dead port, open a fresh one with the same configuration, and run the
    /// login state machine again. After a successful return the shell is
    /// ready to accept commands — no separate `activate()` call needed.
    ///
    /// Recommended pattern after an `Io` error:
    /// ```ignore
    /// match shell.run(&cmd).await {
    ///     Err(ShellError::Io(_)) => {
    ///         shell.reconnect().await?;
    ///         shell.run(&cmd).await
    ///     }
    ///     other => other,
    /// }
    /// ```
    ///
    /// Returns `ShellError::Initialization` if the shell was built without a
    /// port path (test transports), or whatever error the underlying port
    /// open / activate step produces.
    pub async fn reconnect(&mut self) -> Result<(), ShellError> {
        if self.port.is_empty() {
            return Err(ShellError::initialization(
                "cannot reconnect: shell was constructed without a port path \
                 (this is the case for in-memory test transports)",
            ));
        }
        info!(port = %self.port, "reconnecting serial shell");
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
    /// Returns everything the transport has seen on the wire, with ANSI
    /// escape sequences and `\x1f` framing bytes stripped, ready for
    /// human reading. Truncated to the configured `console_buffer_cap`
    /// (default 1 MiB, FIFO trim).
    ///
    /// Particularly useful in error paths: on a failed
    /// [`activate`][crate::shell::Shell::activate], call this to see
    /// exactly what the device emitted on the way to the failure.
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Err(e) = shell.activate().await {
    ///     eprintln!("activate failed: {e}");
    ///     eprintln!("captured console:\n{}", shell.console_buffer());
    /// }
    /// ```
    pub fn console_buffer(&self) -> String {
        self.transport.console_buffer()
    }

    /// Reboot the device and wait for it to come back up.
    ///
    /// Sends `reboot` on the wire, waits out the noisy shutdown +
    /// kernel-reset window (drain-until-silent), then re-runs the
    /// activate state machine to catch the post-boot login prompt.
    /// Returns the total wall-clock duration on success.
    ///
    /// Two timeouts are in play:
    ///
    /// - [`shutdown_timeout`][LinuxSerialShellBuilder::shutdown_timeout]
    ///   caps how long the host waits for the wire to fall silent
    ///   during the shutdown phase (default 30s; bump for devices that
    ///   take minutes to stop services and flush buffers).
    /// - [`login_timeout`][LinuxSerialShellBuilder::login_timeout]
    ///   governs the post-reboot activate (default 120s; bump for slow
    ///   boots).
    ///
    /// # Errors
    ///
    /// - [`ShellError::Initialization`] if the post-reboot activate
    ///   times out before a login prompt appears.
    /// - [`ShellError::Io`] on transport failure during the reboot
    ///   command or subsequent reads.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let elapsed = shell.reboot().await?;
    /// println!("device rebooted in {elapsed:?}");
    /// ```
    pub async fn reboot(&mut self) -> Result<Duration, ShellError> {
        let started = Instant::now();
        debug!("rebooting device");
        self.transport.write_bytes(b"reboot\n").await?;
        self.shell_detected = false;

        // Wait out the noisy transition between the dying shell and the new
        // boot. We consume bytes until the wire is quiet for 2s (signal that
        // the device is mid-reboot or just powered down), capped at 30s.
        //
        // Without this, `activate()` below would race against still-buffered
        // output from the previous shell session and detect a stale prompt,
        // reporting success before the device had actually gone down.
        debug!(
            shutdown_timeout = ?self.shutdown_timeout,
            "waiting for shell to settle (drain_until_silent)",
        );
        self.transport
            .drain_until_silent(SHUTDOWN_SILENCE_THRESHOLD, self.shutdown_timeout)
            .await;

        self.activate().await?;
        let elapsed = started.elapsed();
        info!(?elapsed, "device reboot complete");
        Ok(elapsed)
    }

    /// Send `reboot` to the device and wait for the wire to go silent,
    /// signaling that the system has begun shutting down. Unlike
    /// [`reboot`][Self::reboot], does **not** wait for a new Linux
    /// shell to come back — returns as soon as the line has been quiet
    /// for ~2 seconds (capped at
    /// [`shutdown_timeout`][LinuxSerialShellBuilder::shutdown_timeout]).
    ///
    /// After this returns the shell is in a deactivated state and is
    /// not usable for further commands. The typical pattern is to drop
    /// it immediately and re-open the same port as a
    /// [`UBootSerialShell`][super::UBootSerialShell] to catch the
    /// autoboot countdown.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Io`] on transport failure while sending the
    ///   reboot command.
    ///
    /// # Example
    ///
    /// ```ignore
    /// shell.reboot_no_reactivate().await?;
    /// drop(shell);
    /// let mut uboot = UBootSerialShell::builder(port).open().await?;
    /// uboot.activate().await?;
    /// ```
    pub async fn reboot_no_reactivate(&mut self) -> Result<(), ShellError> {
        debug!("rebooting device (no re-activate)");
        self.transport.write_bytes(b"reboot\n").await?;
        self.shell_detected = false;
        debug!(
            shutdown_timeout = ?self.shutdown_timeout,
            "waiting for shell to settle (drain_until_silent)",
        );
        self.transport
            .drain_until_silent(SHUTDOWN_SILENCE_THRESHOLD, self.shutdown_timeout)
            .await;
        info!("device reboot issued; not waiting for reactivation");
        Ok(())
    }

    /// Power the device off cleanly and wait for the wire to go silent.
    ///
    /// Issues `dmesg -n 5` (best-effort, to quiet kernel chatter
    /// during shutdown) and then `poweroff`. Drains bytes off the wire
    /// until it has been silent for ~2 seconds — signal that userspace
    /// is gone and the kernel has fully halted — capped at the
    /// configured
    /// [`shutdown_timeout`][LinuxSerialShellBuilder::shutdown_timeout]
    /// (default 30s; bump for devices with long shutdown sequences).
    /// Returns the total wall-clock duration.
    ///
    /// After this returns, the device is off and the shell is unusable
    /// — there's no equivalent of [`reboot`][Self::reboot] that brings
    /// it back, because that requires an out-of-band power-on
    /// mechanism the library doesn't have.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Io`] on transport failure while sending the
    ///   shutdown command.
    pub async fn shutdown(&mut self) -> Result<Duration, ShellError> {
        let started = Instant::now();
        debug!(shutdown_timeout = ?self.shutdown_timeout, "shutting down device");
        let _ = self.run(&Command::new("dmesg").args(["-n", "5"])).await;
        self.transport.write_bytes(b"poweroff\n").await?;
        self.shell_detected = false;
        self.transport
            .drain_until_silent(SHUTDOWN_SILENCE_THRESHOLD, self.shutdown_timeout)
            .await;
        let elapsed = started.elapsed();
        info!(?elapsed, "device shutdown complete");
        Ok(elapsed)
    }

    async fn do_login(&mut self) -> Result<(), ShellError> {
        self.transport
            .write_bytes(format!("{}\n", self.username).as_bytes())
            .await?;

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = Vec::new();

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.transport.read_chunk(remaining).await {
                Ok(chunk) => {
                    buf.extend_from_slice(&chunk);
                    if find_subsequence(&buf, b"Password:").is_some() {
                        let password = self.password.as_ref().ok_or_else(|| {
                            ShellError::initialization(
                                "device asked for a password but none was configured",
                            )
                        })?;
                        self.transport
                            .write_bytes(format!("{}\n", password).as_bytes())
                            .await?;
                        return Ok(());
                    }
                    if self.shell_prompt.find(&buf).is_some() {
                        self.shell_detected = true;
                        return Ok(());
                    }
                }
                Err(_) => return Ok(()),
            }
        }
        Ok(())
    }

    async fn cleanup(&mut self) -> Result<(), ShellError> {
        let _ = self.transport.write_bytes(b"dmesg -n5\n").await;
        let _ = self.transport.write_bytes(b"stty echo\n").await;
        Ok(())
    }

    async fn exec_framed(&mut self, command: &Command) -> Result<ShellResult, ShellError> {
        let _ = self.transport.drain(Duration::from_millis(50)).await;

        let cwd_set = if let Some(cwd) = command.cwd_path() {
            debug!(?cwd, "pushd");
            self.transport
                .write_bytes(format!("pushd {}\n", cwd.display()).as_bytes())
                .await?;
            let _ = self.transport.drain(Duration::from_millis(100)).await;
            true
        } else {
            false
        };

        let wire = command.wire_string();
        let timeout = command.timeout_duration();
        let framed = format!(
            "(timeout {} {}) > /tmp/out 2> /tmp/err; echo -e \"$(echo $?)\\x1f\\n$(cat /tmp/out)\\x1f\\n$(cat /tmp/err)\\x1f\"\n",
            timeout.as_secs_f64(),
            wire,
        );
        trace!(framed = %framed.trim_end(), "sending framed command");

        let started = Utc::now();
        self.transport.write_bytes(framed.as_bytes()).await?;
        let response = self
            .transport
            .read_until(triple_sentinel, timeout + Duration::from_secs(2))
            .await?;

        let (exit_code, stdout, stderr) = parse_framed_response(&response)?;

        if cwd_set {
            self.transport.write_bytes(b"popd\n").await?;
            let _ = self.transport.drain(Duration::from_millis(100)).await;
        }

        let result = Box::new(ShellResult::new(
            wire.clone(),
            stdout,
            stderr,
            exit_code,
            started,
        ));

        if exit_code == 124 {
            return Err(ShellError::Timeout {
                duration: timeout,
                result,
            });
        }
        if exit_code == 127 {
            return Err(ShellError::CommandNotFound {
                command: command.base().to_string(),
                result,
            });
        }
        if exit_code != 0 && !command.allows_nonzero() {
            return Err(ShellError::CommandFailed(result));
        }

        Ok(*result)
    }

    async fn run_with_recovery(&mut self, command: &Command) -> Result<ShellResult, ShellError> {
        let first = self.exec_framed(command).await;
        match &first {
            Ok(_)
            | Err(ShellError::CommandFailed(_))
            | Err(ShellError::CommandNotFound { .. })
            | Err(ShellError::Timeout { .. }) => return first,
            _ => {}
        }

        let original = first.unwrap_err();
        warn!(error = %original, "shell exec failed, attempting recovery");

        let _ = self.transport.write_bytes(b"\x03").await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ = self.transport.drain(Duration::from_millis(100)).await;

        let probe = Command::new("true").timeout(Duration::from_secs(1));
        match self.exec_framed(&probe).await {
            Ok(_) => Err(original),
            Err(_) => {
                warn!("shell unresponsive, re-activating");
                let _ = self.transport.write_bytes(b"\x03").await;
                tokio::time::sleep(Duration::from_millis(500)).await;
                self.shell_detected = false;
                self.activate().await?;
                self.exec_framed(command).await
            }
        }
    }
}

#[async_trait]
impl Shell for LinuxSerialShell {
    async fn activate(&mut self) -> Result<(), ShellError> {
        let started = Instant::now();
        debug!("probing linux serial shell");
        let mut accumulated: Vec<u8> = Vec::new();

        while !self.shell_detected && started.elapsed() < self.login_timeout {
            let remaining = self.login_timeout.saturating_sub(started.elapsed());
            let poll = PROMPT_POLL.min(remaining);

            match self.transport.read_chunk(poll).await {
                Ok(chunk) => {
                    accumulated.extend_from_slice(&chunk);
                    cap_buffer(&mut accumulated, ACTIVATE_BUFFER_CAP);

                    if self.login_prompt.find(&accumulated).is_some() {
                        debug!("login prompt detected");
                        self.do_login().await?;
                        accumulated.clear();
                    } else if self.shell_prompt.find(&accumulated).is_some() {
                        debug!("shell prompt detected");
                        self.shell_detected = true;
                        break;
                    } else if prompts::find_uboot_shell(&accumulated).is_some() {
                        debug!("u-boot prompt detected, resetting to boot linux");
                        self.transport.write_bytes(b"reset\n").await?;
                        tokio::time::sleep(Duration::from_secs(20)).await;
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
                "login timed out after {:?}",
                self.login_timeout
            )));
        }

        // Quiet down kernel chatter and disable terminal echo so framing parses
        // cleanly. The drain at the end swallows any output produced by the
        // commands themselves.
        self.transport.write_bytes(b"dmesg -n1\n").await?;
        self.transport.write_bytes(b"stty echo\n").await?;
        self.transport
            .write_bytes(b"stty -echo rows 9999 cols 9999\n")
            .await?;
        let _ = self.transport.drain(Duration::from_millis(200)).await;

        info!(
            port = %self.port,
            username = %self.username,
            "linux serial shell activated"
        );
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<(), ShellError> {
        if self.shell_detected {
            let _ = self.cleanup().await;
            self.transport.close().await;
            self.shell_detected = false;
        }
        Ok(())
    }

    async fn run(&mut self, command: &Command) -> Result<ShellResult, ShellError> {
        self.run_with_recovery(command).await
    }
}

/// Predicate for `read_until`: returns the offset just past the third
/// `\x1f` byte in `buf`, or `None` if fewer than three are present.
fn triple_sentinel(buf: &[u8]) -> Option<usize> {
    let mut count = 0;
    for (i, &b) in buf.iter().enumerate() {
        if b == SENTINEL {
            count += 1;
            if count == 3 {
                return Some(i + 1);
            }
        }
    }
    None
}

fn cap_buffer(buf: &mut Vec<u8>, max: usize) {
    if buf.len() > max {
        let drop = buf.len() - max;
        buf.drain(..drop);
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_framed_response(buf: &[u8]) -> Result<(i32, Option<String>, Option<String>), ShellError> {
    let segments: Vec<&[u8]> = buf.split(|&b| b == SENTINEL).collect();
    if segments.len() < 3 {
        return Err(ShellError::initialization(format!(
            "malformed exec response: expected at least 3 sentinel-separated segments, got {}",
            segments.len()
        )));
    }
    let exit_code = parse_exit_code(segments[0])?;
    let stdout = strip_segment(segments[1]);
    let stderr = strip_segment(segments[2]);
    Ok((exit_code, stdout, stderr))
}

fn parse_exit_code(segment: &[u8]) -> Result<i32, ShellError> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"-?\d+").unwrap());
    let s = String::from_utf8_lossy(segment);
    re.find_iter(&s)
        .last()
        .and_then(|m| m.as_str().parse::<i32>().ok())
        .ok_or_else(|| ShellError::initialization(format!("could not parse exit code in {s:?}")))
}

/// Trim the single leading `\r\n` (echo's `\n` separator after the previous
/// sentinel) and a single trailing `\r\n` (terminal newline at end of segment).
/// Returns `None` if the segment is empty after trimming.
fn strip_segment(buf: &[u8]) -> Option<String> {
    let owned = String::from_utf8_lossy(buf).into_owned();
    let s = owned.strip_prefix("\r\n").unwrap_or(&owned);
    let s = s.strip_suffix("\r\n").unwrap_or(s);
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    // ---- parser unit tests (no transport) ----

    #[test]
    fn triple_sentinel_finds_position_after_third() {
        let buf = b"a\x1fb\x1fc\x1fd";
        assert_eq!(triple_sentinel(buf), Some(6));
    }

    #[test]
    fn triple_sentinel_returns_none_below_three() {
        assert_eq!(triple_sentinel(b"a\x1fb\x1f"), None);
    }

    #[test]
    fn parse_exit_code_picks_last_integer() {
        assert_eq!(parse_exit_code(b"0").unwrap(), 0);
        assert_eq!(parse_exit_code(b"prefix garbage 42").unwrap(), 42);
        assert_eq!(parse_exit_code(b"old 1\n2\n3").unwrap(), 3);
    }

    #[test]
    fn parse_exit_code_handles_negative() {
        assert_eq!(parse_exit_code(b"-1").unwrap(), -1);
    }

    #[test]
    fn parse_exit_code_errors_with_no_digits() {
        assert!(parse_exit_code(b"no digits here").is_err());
    }

    #[test]
    fn strip_segment_trims_one_leading_and_trailing_crlf() {
        assert_eq!(strip_segment(b"\r\nhello\r\n"), Some("hello".to_string()));
    }

    #[test]
    fn strip_segment_preserves_internal_crlf() {
        assert_eq!(
            strip_segment(b"\r\nhello\r\nworld"),
            Some("hello\r\nworld".to_string())
        );
    }

    #[test]
    fn strip_segment_returns_none_for_empty() {
        assert_eq!(strip_segment(b""), None);
        assert_eq!(strip_segment(b"\r\n"), None);
    }

    #[test]
    fn parse_framed_response_basic_success() {
        // exit=0, stdout="hello", stderr=empty
        let buf = b"0\x1f\r\nhello\x1f\r\n\x1f";
        let (exit, out, err) = parse_framed_response(buf).unwrap();
        assert_eq!(exit, 0);
        assert_eq!(out, Some("hello".to_string()));
        assert_eq!(err, None);
    }

    #[test]
    fn parse_framed_response_with_stderr() {
        let buf = b"2\x1f\x1f\r\nbad\x1f";
        let (exit, out, err) = parse_framed_response(buf).unwrap();
        assert_eq!(exit, 2);
        assert_eq!(out, None);
        assert_eq!(err, Some("bad".to_string()));
    }

    #[test]
    fn parse_framed_response_multiline_stdout() {
        let buf = b"0\x1f\r\nline1\r\nline2\x1f\r\n\x1f";
        let (_, out, _) = parse_framed_response(buf).unwrap();
        assert_eq!(out, Some("line1\r\nline2".to_string()));
    }

    #[test]
    fn parse_framed_response_too_few_segments() {
        let err = parse_framed_response(b"0\x1f").unwrap_err();
        assert!(matches!(err, ShellError::Initialization(_)));
    }

    // ---- transport-level integration tests using tokio::io::duplex ----

    fn pre_activated() -> (LinuxSerialShell, tokio::io::DuplexStream) {
        let (host, device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = LinuxSerialShell::from_transport(transport);
        shell.shell_detected = true;
        (shell, device)
    }

    /// Spawn a task that drains anything the shell writes, then writes
    /// the supplied canned response back. Returns the JoinHandle.
    fn respond_with(
        mut device: tokio::io::DuplexStream,
        response: Vec<u8>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut sink = vec![0u8; 4096];
            // Wait briefly for the shell to send its command.
            let _ = tokio::time::timeout(Duration::from_millis(200), device.read(&mut sink)).await;
            device.write_all(&response).await.unwrap();
            device.flush().await.unwrap();
            // Keep device alive long enough for the shell to finish reading.
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
    }

    #[tokio::test]
    async fn run_basic_command_with_stdout() {
        let (mut shell, device) = pre_activated();
        let device_task = respond_with(device, b"0\x1f\r\nhello\x1f\r\n\x1f\r\n".to_vec());
        let r = shell.run(&Command::new("echo").arg("hello")).await.unwrap();
        assert_eq!(r.exit_code(), 0);
        assert_eq!(r.stdout(), Some("hello"));
        assert!(r.stderr().is_none());
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn run_command_with_stderr_and_nonzero_exit() {
        let (mut shell, device) = pre_activated();
        let device_task = respond_with(device, b"2\x1f\x1f\r\nsomething bad\x1f\r\n".to_vec());
        let err = shell.run(&Command::new("false")).await.unwrap_err();
        match err {
            ShellError::CommandFailed(r) => {
                assert_eq!(r.exit_code(), 2);
                assert_eq!(r.stderr(), Some("something bad"));
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn run_command_with_allow_nonzero_returns_ok() {
        let (mut shell, device) = pre_activated();
        let device_task = respond_with(device, b"1\x1f\x1f\x1f\r\n".to_vec());
        let r = shell
            .run(&Command::new("false").allow_nonzero())
            .await
            .unwrap();
        assert_eq!(r.exit_code(), 1);
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn run_command_exit_124_maps_to_timeout() {
        let (mut shell, device) = pre_activated();
        let device_task = respond_with(device, b"124\x1f\x1f\x1f\r\n".to_vec());
        let err = shell
            .run(&Command::new("sleep").arg("10"))
            .await
            .unwrap_err();
        assert!(matches!(err, ShellError::Timeout { .. }));
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn run_command_exit_127_maps_to_not_found() {
        let (mut shell, device) = pre_activated();
        let device_task = respond_with(
            device,
            b"127\x1f\x1f\r\n/bin/sh: nope: not found\x1f\r\n".to_vec(),
        );
        let err = shell.run(&Command::new("nope")).await.unwrap_err();
        match err {
            ShellError::CommandNotFound { command, .. } => {
                assert_eq!(command, "nope");
            }
            other => panic!("expected CommandNotFound, got {other:?}"),
        }
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn activate_with_shell_prompt_already_present() {
        let (host, mut device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = LinuxSerialShell::from_transport(transport);

        // Feed a shell prompt directly.
        device.write_all(b"\r\nroot@device:~# ").await.unwrap();
        device.flush().await.unwrap();

        // Keep device drained so the stty commands don't block.
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
    async fn activate_runs_full_login_flow() {
        let (host, mut device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = LinuxSerialShell::from_transport(transport);
        shell.password = Some("secret".to_string());

        let device_task = tokio::spawn(async move {
            // 1. Present login prompt.
            device.write_all(b"\r\ndevice login: ").await.unwrap();

            // 2. Expect "root\n" from the shell.
            let mut buf = [0u8; 5];
            device.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"root\n");

            // 3. Present password prompt.
            device.write_all(b"\r\nPassword: ").await.unwrap();

            // 4. Expect "secret\n".
            let mut pw = [0u8; 7];
            device.read_exact(&mut pw).await.unwrap();
            assert_eq!(&pw, b"secret\n");

            // 5. Grant the shell prompt.
            device.write_all(b"\r\nroot@device:~# ").await.unwrap();

            // 6. Drain the stty / dmesg setup commands.
            let mut sink = vec![0u8; 4096];
            let _ = tokio::time::timeout(Duration::from_millis(300), device.read(&mut sink)).await;
        });

        shell.activate().await.unwrap();
        assert!(shell.shell_detected);
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn activate_times_out_with_no_prompt() {
        let (host, _device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = LinuxSerialShell::from_transport(transport);
        shell.login_timeout = Duration::from_millis(300);

        let err = shell.activate().await.unwrap_err();
        assert!(matches!(err, ShellError::Initialization(_)));
    }

    // ---- builder + custom prompt tests ----

    #[test]
    fn builder_defaults() {
        let b = LinuxSerialShell::builder("/dev/ttyUSB0");
        assert_eq!(b.port, "/dev/ttyUSB0");
        assert_eq!(b.baudrate, DEFAULT_BAUDRATE);
        assert_eq!(b.username, DEFAULT_USERNAME);
        assert_eq!(b.password, None);
        assert_eq!(b.login_timeout, DEFAULT_LOGIN_TIMEOUT);
        assert_eq!(b.shutdown_timeout, DEFAULT_SHUTDOWN_TIMEOUT);
        assert_eq!(b.console_buffer_cap, DEFAULT_CONSOLE_BUFFER_CAP);
    }

    #[test]
    fn builder_setters_chain() {
        let b = LinuxSerialShell::builder("/dev/ttyUSB0")
            .baudrate(57600)
            .username("admin")
            .password("secret")
            .login_timeout(Duration::from_secs(30))
            .shutdown_timeout(Duration::from_secs(180))
            .console_buffer_cap(2048);
        assert_eq!(b.baudrate, 57600);
        assert_eq!(b.username, "admin");
        assert_eq!(b.password.as_deref(), Some("secret"));
        assert_eq!(b.login_timeout, Duration::from_secs(30));
        assert_eq!(b.shutdown_timeout, Duration::from_secs(180));
        assert_eq!(b.console_buffer_cap, 2048);
    }

    #[tokio::test]
    async fn activate_detects_custom_shell_prompt() {
        let (host, mut device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = LinuxSerialShell::builder("dummy")
            .shell_prompt(r"yocto-dev:[^#]*#")
            .build_with_transport(transport)
            .unwrap();

        device.write_all(b"\r\nyocto-dev:/tmp# ").await.unwrap();
        device.flush().await.unwrap();

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
    async fn activate_with_custom_login_prompt() {
        let (host, mut device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = LinuxSerialShell::builder("dummy")
            .login_prompt(r"please log in:")
            .password("pw")
            .build_with_transport(transport)
            .unwrap();

        let device_task = tokio::spawn(async move {
            device.write_all(b"\r\nplease log in: ").await.unwrap();
            let mut buf = [0u8; 5];
            device.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"root\n");
            device.write_all(b"\r\nPassword: ").await.unwrap();
            let mut pw = [0u8; 3];
            device.read_exact(&mut pw).await.unwrap();
            assert_eq!(&pw, b"pw\n");
            device.write_all(b"\r\nroot@device:~# ").await.unwrap();
            let mut sink = vec![0u8; 4096];
            let _ = tokio::time::timeout(Duration::from_millis(300), device.read(&mut sink)).await;
        });

        shell.activate().await.unwrap();
        assert!(shell.shell_detected);
        let _ = device_task.await;
    }

    #[tokio::test]
    async fn bad_shell_prompt_regex_surfaces_as_invalid_regex_error() {
        let (host, _device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let result = LinuxSerialShell::builder("dummy")
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

    #[tokio::test]
    async fn reconnect_errors_without_port_configured() {
        let (host, _device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let mut shell = LinuxSerialShell::from_transport(transport);
        let err = shell.reconnect().await.unwrap_err();
        assert!(matches!(err, ShellError::Initialization(_)));
    }

    #[tokio::test]
    async fn bad_login_prompt_regex_surfaces_as_invalid_regex_error() {
        let (host, _device) = tokio::io::duplex(8192);
        let transport = SerialTransport::new(host);
        let result = LinuxSerialShell::builder("dummy")
            .login_prompt(r"[")
            .build_with_transport(transport);
        match result {
            Err(ShellError::InvalidRegex { .. }) => {}
            Err(other) => panic!("expected InvalidRegex, got {other:?}"),
            Ok(_) => panic!("expected an error, builder accepted the bad regex"),
        }
    }
}
