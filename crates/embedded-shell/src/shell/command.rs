use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default per-command timeout (5 seconds).
///
/// Every command runs under a device-side `timeout(1)` wrapper using
/// this duration unless overridden via [`Command::timeout`]. The
/// wrapper is mandatory — there's no way to disable it — because on a
/// one-way serial line the host can't reliably kill a stalled
/// device-side process. A bounded deadline is the only safe contract.
pub const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(5);

/// A single command to run on a [`Shell`][super::Shell].
///
/// Built fluently via [`Command::new`] + [`Command::arg`] /
/// [`Command::args`], with optional [`Command::timeout`],
/// [`Command::cwd`], and [`Command::allow_nonzero`]. The result is
/// passed by reference to [`Shell::run`][super::Shell::run].
///
/// # Argv-only by design
///
/// Commands are always argv-style — a binary plus separate argument
/// tokens. Arguments are POSIX-quoted before reaching the wire, so
/// paths with spaces and shell metacharacters are safe by default; no
/// command-injection risk from interpolating user input into argv.
///
/// To use shell features (pipes, redirects, env-var prefixes, `&&`),
/// **opt in explicitly** by spawning `sh -c`:
///
/// ```ignore
/// use embedded_shell::shell::Command;
///
/// // safe, no quoting concerns
/// let c = Command::new("ls").arg("-la").arg("/tmp/with spaces");
///
/// // shell features — quoting responsibility is on the caller of `sh -c`,
/// // visible at the call site to anyone reviewing the code
/// let c = Command::new("sh").args(["-c", "ls /tmp | head -1 > /tmp/out"]);
/// ```
#[derive(Debug, Clone)]
pub struct Command {
    argv: Vec<String>,
    timeout: Duration,
    cwd: Option<PathBuf>,
    allow_nonzero: bool,
}

impl Command {
    /// Begin a new command with `binary` as `argv[0]`.
    ///
    /// Defaults to a 5-second timeout, no `cwd` override, and erroring
    /// on non-zero exit.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use embedded_shell::shell::Command;
    ///
    /// let c = Command::new("uname");
    /// ```
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            argv: vec![binary.into()],
            timeout: DEFAULT_EXEC_TIMEOUT,
            cwd: None,
            allow_nonzero: false,
        }
    }

    /// Append one argument to the argv list.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use embedded_shell::shell::Command;
    ///
    /// let c = Command::new("uname").arg("-a");
    /// ```
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.argv.push(arg.into());
        self
    }

    /// Append multiple arguments at once. Equivalent to chaining
    /// [`arg`][Self::arg] for each element.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use embedded_shell::shell::Command;
    ///
    /// let c = Command::new("ping").args(["-c", "1", "8.8.8.8"]);
    /// ```
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.argv.extend(args.into_iter().map(Into::into));
        self
    }

    /// Override the per-command timeout.
    ///
    /// The device-side `timeout(1)` wrapper uses this duration. There is
    /// no way to disable the wrapper; pass a very large duration if you
    /// genuinely need long-running commands.
    ///
    /// # Example
    ///
    /// ```ignore
    /// # use std::time::Duration;
    /// use embedded_shell::shell::Command;
    ///
    /// let c = Command::new("sleep").arg("60").timeout(Duration::from_secs(90));
    /// ```
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the working directory the command runs in.
    ///
    /// Honoured by shells that support `cd` (notably
    /// [`SubprocessShell`][super::SubprocessShell] and
    /// [`LinuxSerialShell`][super::LinuxSerialShell]). Ignored by
    /// [`UBootSerialShell`][super::UBootSerialShell], which has no
    /// concept of a working directory.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use embedded_shell::shell::Command;
    ///
    /// let c = Command::new("pwd").cwd("/tmp");
    /// ```
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Don't error on non-zero exit codes.
    ///
    /// By default, [`Shell::run`][super::Shell::run] returns
    /// [`ShellError::CommandFailed`][super::ShellError::CommandFailed]
    /// when the command exits non-zero. With `allow_nonzero` set, the
    /// caller gets `Ok(ShellResult)` and can inspect
    /// [`ShellResult::exit_code`][super::ShellResult::exit_code]
    /// themselves. [`ShellError::Timeout`][super::ShellError::Timeout]
    /// and [`ShellError::CommandNotFound`][super::ShellError::CommandNotFound]
    /// are *not* suppressed — those still error.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use embedded_shell::shell::Command;
    ///
    /// // `false` exits 1; allow_nonzero turns that into Ok with exit_code=1
    /// let c = Command::new("false").allow_nonzero();
    /// ```
    pub fn allow_nonzero(mut self) -> Self {
        self.allow_nonzero = true;
        self
    }

    /// The configured per-command timeout.
    pub fn timeout_duration(&self) -> Duration {
        self.timeout
    }

    /// The configured working directory, if any.
    pub fn cwd_path(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Whether [`allow_nonzero`][Self::allow_nonzero] was set.
    pub fn allows_nonzero(&self) -> bool {
        self.allow_nonzero
    }

    /// The wire-format string this command produces.
    ///
    /// Returns the exact bytes that the [`Shell`][super::Shell]
    /// implementation will write to the device's shell prompt (or wrap
    /// in `sh -c` for [`SubprocessShell`][super::SubprocessShell]).
    /// Argument tokens are POSIX-quoted as needed; safe characters
    /// (`A-Z0-9_-./:=+,@%`) are passed verbatim.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use embedded_shell::shell::Command;
    ///
    /// let c = Command::new("ls").arg("/tmp/with spaces");
    /// assert_eq!(c.wire_string(), "ls '/tmp/with spaces'");
    /// ```
    pub fn wire_string(&self) -> String {
        self.argv
            .iter()
            .map(|s| posix_quote(s))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Binary name (`argv[0]`) — used in error messages.
    pub(crate) fn base(&self) -> &str {
        self.argv.first().map(|s| s.as_str()).unwrap_or("")
    }
}

/// POSIX-shell quote per shlex rules: pass alnum/-/_/./:/=/+/,/@/% verbatim,
/// otherwise wrap in single quotes with internal `'` escaped as `'\''`.
fn posix_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '+' | ',' | '@' | '%')
    });
    if safe {
        return s.to_string();
    }
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
    use super::*;

    #[test]
    fn wire_string_joins_argv_with_spaces() {
        let c = Command::new("ls").arg("-la").arg("/tmp");
        assert_eq!(c.wire_string(), "ls -la /tmp");
    }

    #[test]
    fn wire_string_quotes_args_with_spaces() {
        let c = Command::new("ls").arg("/tmp/dir with spaces");
        assert_eq!(c.wire_string(), "ls '/tmp/dir with spaces'");
    }

    #[test]
    fn wire_string_quotes_args_with_shell_metacharacters() {
        let c = Command::new("echo").arg("a;b|c&d");
        assert_eq!(c.wire_string(), "echo 'a;b|c&d'");
    }

    #[test]
    fn wire_string_handles_embedded_single_quote() {
        let c = Command::new("echo").arg("it's");
        assert_eq!(c.wire_string(), "echo 'it'\\''s'");
    }

    #[test]
    fn wire_string_quotes_url_with_query_params() {
        let c = Command::new("curl").arg("https://example.com/path?q=1&a=2");
        // ? and & are unsafe — get quoted
        let wire = c.wire_string();
        assert!(wire.starts_with("curl "));
        assert!(wire.contains("'https://example.com/path?q=1&a=2'"));
    }

    #[test]
    fn args_appends_multiple() {
        let c = Command::new("ping").args(["-c", "1", "8.8.8.8"]);
        assert_eq!(c.wire_string(), "ping -c 1 8.8.8.8");
    }

    #[test]
    fn sh_c_form_for_shell_features() {
        // The canonical way to get pipes/redirects: explicit sh -c.
        let c = Command::new("sh").args(["-c", "ls /tmp | head -1"]);
        // The inner string is unsafe (contains spaces, pipe), so it gets quoted
        // exactly once as a single argv slot.
        assert_eq!(c.wire_string(), "sh -c 'ls /tmp | head -1'");
    }

    #[test]
    fn base_returns_binary() {
        assert_eq!(Command::new("ls").arg("-la").base(), "ls");
    }

    #[test]
    fn timeout_cwd_allow_nonzero_are_persisted() {
        let c = Command::new("true")
            .timeout(Duration::from_secs(30))
            .cwd("/tmp")
            .allow_nonzero();
        assert_eq!(c.timeout_duration(), Duration::from_secs(30));
        assert_eq!(c.cwd_path(), Some(Path::new("/tmp")));
        assert!(c.allows_nonzero());
    }

    #[test]
    fn default_timeout_is_five_seconds() {
        assert_eq!(
            Command::new("true").timeout_duration(),
            DEFAULT_EXEC_TIMEOUT
        );
    }
}
