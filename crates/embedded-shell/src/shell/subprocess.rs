//! Local subprocess shell — runs commands on the host machine.
//!
//! This is the reference [`Shell`] implementation: it shells out to a
//! local `sh -c` rather than driving a remote device. Useful for tests
//! of higher-level wrappers (the `embedded-shell-linux::fs::*`
//! functions, for example, are tested against `SubprocessShell` against
//! the host's `/tmp`), and for any host-side scripting that benefits
//! from the same [`Command`] / [`ShellResult`] / [`ShellError`] shape
//! as the serial backends.
//!
//! Like the serial shells, every command is wrapped in `timeout <n>
//! <cmd>` so the GNU coreutils `timeout(1)` binary enforces the
//! deadline (yielding exit code 124 on expiry).
//!
//! [`Shell`]: crate::shell::Shell
//! [`Command`]: crate::shell::Command
//! [`ShellResult`]: crate::shell::ShellResult
//! [`ShellError`]: crate::shell::ShellError

use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use chrono::Utc;
use tokio::process::Command as TokioCommand;
use tracing::debug;

use super::command::Command;
use super::error::ShellError;
use super::result::ShellResult;
use super::traits::{LinuxShell, Shell};

/// [`Shell`] implementation that runs commands on the local host.
///
/// Stateless and has no lifecycle of its own: `activate` and
/// `deactivate` are no-ops. Construct with [`SubprocessShell::new`] (or
/// `SubprocessShell::default()`), then use it like any other
/// [`Shell`].
///
/// # Example
///
/// ```ignore
/// use embedded_shell::shell::{Command, Shell, SubprocessShell};
///
/// let mut shell = SubprocessShell::new();
/// let r = shell.run(&Command::new("uname").arg("-a")).await?;
/// println!("{}", r.stdout().unwrap_or(""));
/// ```
///
/// [`Shell`]: crate::shell::Shell
#[derive(Debug, Default, Clone)]
pub struct SubprocessShell;

impl LinuxShell for SubprocessShell {}

impl SubprocessShell {
    /// Construct a new local-subprocess shell. Equivalent to
    /// `SubprocessShell::default()`.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Shell for SubprocessShell {
    async fn activate(&mut self) -> Result<(), ShellError> {
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<(), ShellError> {
        Ok(())
    }

    async fn run(&mut self, command: &Command) -> Result<ShellResult, ShellError> {
        let wire = command.wire_string();
        debug!(cmd = %wire, "executing");

        let wrapped = format!(
            "timeout {} {}",
            command.timeout_duration().as_secs_f64(),
            wire
        );

        let started = Utc::now();
        let output = TokioCommand::new("sh")
            .arg("-c")
            .arg(&wrapped)
            .current_dir(command.cwd_path().unwrap_or_else(|| Path::new(".")))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        let exit_code = output
            .status
            .code()
            .ok_or_else(|| ShellError::initialization("process terminated by signal"))?;

        let stdout = decode_stream(&output.stdout);
        let stderr = decode_stream(&output.stderr);

        let result = Box::new(ShellResult::new(
            wire.clone(),
            stdout,
            stderr,
            exit_code,
            started,
        ));

        if exit_code == 124 {
            return Err(ShellError::Timeout {
                duration: command.timeout_duration(),
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
}

fn decode_stream(buf: &[u8]) -> Option<String> {
    if buf.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(buf).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::shell::error::ShellError;

    #[tokio::test]
    async fn activate_deactivate_are_noops() {
        let mut shell = SubprocessShell::new();
        shell.activate().await.unwrap();
        shell.deactivate().await.unwrap();
    }

    #[tokio::test]
    async fn echo_captures_stdout() {
        let mut shell = SubprocessShell::new();
        let r = shell.run(&Command::new("echo").arg("hello")).await.unwrap();
        assert_eq!(r.exit_code(), 0);
        assert_eq!(r.stdout(), Some("hello\n"));
        assert!(r.stderr().is_none());
        assert!(r.is_success());
    }

    #[tokio::test]
    async fn stderr_is_captured_separately() {
        let mut shell = SubprocessShell::new();
        // `1>&2` is a shell redirect — needs explicit `sh -c`.
        let r = shell
            .run(&Command::new("sh").args(["-c", "echo oops 1>&2"]))
            .await
            .unwrap();
        assert!(r.stdout().is_none());
        assert_eq!(r.stderr(), Some("oops\n"));
    }

    #[tokio::test]
    async fn argv_quotes_args_with_spaces() {
        let mut shell = SubprocessShell::new();
        let r = shell
            .run(&Command::new("echo").arg("hello world"))
            .await
            .unwrap();
        assert_eq!(r.stdout(), Some("hello world\n"));
    }

    #[tokio::test]
    async fn pipes_via_explicit_sh_c() {
        let mut shell = SubprocessShell::new();
        let r = shell
            .run(&Command::new("sh").args(["-c", "echo one two three | wc -w"]))
            .await
            .unwrap();
        assert_eq!(r.stdout().unwrap().trim(), "3");
    }

    #[tokio::test]
    async fn nonzero_exit_yields_command_failed() {
        let mut shell = SubprocessShell::new();
        let err = shell.run(&Command::new("false")).await.unwrap_err();
        match err {
            ShellError::CommandFailed(r) => assert_eq!(r.exit_code(), 1),
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn allow_nonzero_returns_ok_on_nonzero_exit() {
        let mut shell = SubprocessShell::new();
        let r = shell
            .run(&Command::new("false").allow_nonzero())
            .await
            .unwrap();
        assert_eq!(r.exit_code(), 1);
        assert!(!r.is_success());
    }

    #[tokio::test]
    async fn allow_nonzero_does_not_swallow_command_not_found() {
        let mut shell = SubprocessShell::new();
        let err = shell
            .run(&Command::new("definitely-not-a-real-binary-xyz").allow_nonzero())
            .await
            .unwrap_err();
        assert!(matches!(err, ShellError::CommandNotFound { .. }));
    }

    #[tokio::test]
    async fn allow_nonzero_does_not_swallow_timeout() {
        let mut shell = SubprocessShell::new();
        let err = shell
            .run(
                &Command::new("sleep")
                    .arg("5")
                    .timeout(Duration::from_millis(200))
                    .allow_nonzero(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ShellError::Timeout { .. }));
    }

    #[tokio::test]
    async fn command_not_found_maps_to_127() {
        let mut shell = SubprocessShell::new();
        let err = shell
            .run(&Command::new("definitely-not-a-real-binary-xyz"))
            .await
            .unwrap_err();
        match err {
            ShellError::CommandNotFound { command, result } => {
                assert_eq!(command, "definitely-not-a-real-binary-xyz");
                assert_eq!(result.exit_code(), 127);
            }
            other => panic!("expected CommandNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_yields_timeout_variant() {
        let mut shell = SubprocessShell::new();
        let err = shell
            .run(
                &Command::new("sleep")
                    .arg("5")
                    .timeout(Duration::from_millis(200)),
            )
            .await
            .unwrap_err();
        match err {
            ShellError::Timeout { duration, result } => {
                assert_eq!(duration, Duration::from_millis(200));
                assert_eq!(result.exit_code(), 124);
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cwd_is_honoured() {
        let mut shell = SubprocessShell::new();
        let r = shell.run(&Command::new("pwd").cwd("/tmp")).await.unwrap();
        assert_eq!(r.stdout(), Some("/tmp\n"));
    }

    #[tokio::test]
    async fn duration_is_populated() {
        let mut shell = SubprocessShell::new();
        let r = shell.run(&Command::new("true")).await.unwrap();
        assert!(r.duration() >= chrono::Duration::zero());
        assert!(r.finished() >= r.started());
    }
}
