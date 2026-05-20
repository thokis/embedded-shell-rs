use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Serialize;

/// The outcome of running one [`Command`][super::Command] on a
/// [`Shell`][super::Shell].
///
/// Carries the command's stdout, stderr, exit code, and timing
/// information. Constructed by `Shell` implementations and returned to
/// callers either directly (on success) or wrapped inside
/// [`ShellError`][super::ShellError] variants (on failure modes that
/// still produced meaningful output, like `CommandFailed` or `Timeout`).
///
/// # Field semantics
///
/// - **`stdout`**, **`stderr`**: `None` when the stream was empty,
///   `Some(s)` otherwise. `\r` bytes are stripped at construction (a
///   serial-line `\r\n` becomes a clean `\n`), so callers don't need
///   to normalise line endings.
/// - **`exit_code`**: the device-side exit code. `0` on success,
///   `124` for `timeout(1)` kills (Linux shells), `127` for
///   command-not-found.
/// - **`started`** / **`finished`**: UTC timestamps captured by the
///   `Shell` implementation just before sending the wire bytes and
///   just after parsing the response.
///
/// # Regex helpers
///
/// The `re_*` methods are convenience accessors for pulling structured
/// data out of stdout via a regex. They all share the same contract:
///
/// - Return `Option<...>` (or `Vec<String>` for [`re_findall`][Self::re_findall]) —
///   no match is a normal outcome, not an error.
/// - **Panic** if the regex pattern fails to compile. Patterns are
///   expected to be literal strings in caller code; a malformed
///   pattern is a programmer bug, surfaced loudly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellResult {
    command: String,
    stdout: Option<String>,
    stderr: Option<String>,
    exit_code: i32,
    started: DateTime<Utc>,
    finished: DateTime<Utc>,
}

impl ShellResult {
    /// Construct a result with the given fields. `finished` is set to
    /// the current UTC time. `stdout` has `\r` bytes stripped.
    ///
    /// Intended for `Shell` implementors; application code receives
    /// already-constructed instances from
    /// [`Shell::run`][super::Shell::run].
    pub fn new(
        command: impl Into<String>,
        stdout: Option<String>,
        stderr: Option<String>,
        exit_code: i32,
        started: DateTime<Utc>,
    ) -> Self {
        Self {
            command: command.into(),
            stdout: stdout.map(strip_cr),
            stderr,
            exit_code,
            started,
            finished: Utc::now(),
        }
    }

    /// The wire-format string that was sent to the device.
    ///
    /// Includes any POSIX quoting applied by the
    /// [`Command`][super::Command] builder; this is the exact text the
    /// device's shell received.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// The captured standard output, with `\r` bytes stripped. `None`
    /// if the command produced no output on stdout.
    pub fn stdout(&self) -> Option<&str> {
        self.stdout.as_deref()
    }

    /// The captured standard error. `None` if the command produced no
    /// output on stderr, or for backends that don't separate streams
    /// (notably [`UBootSerialShell`][super::UBootSerialShell]).
    pub fn stderr(&self) -> Option<&str> {
        self.stderr.as_deref()
    }

    /// The device-side exit code. `0` on success, `124` for
    /// `timeout(1)` kills (Linux shells), `127` for command-not-found.
    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// UTC timestamp captured just before the wire bytes were sent.
    pub fn started(&self) -> DateTime<Utc> {
        self.started
    }

    /// UTC timestamp captured just after the response was parsed.
    pub fn finished(&self) -> DateTime<Utc> {
        self.finished
    }

    /// Wall-clock duration from [`started`][Self::started] to
    /// [`finished`][Self::finished], as a [`chrono::Duration`].
    pub fn duration(&self) -> chrono::Duration {
        self.finished - self.started
    }

    /// Wall-clock duration as fractional seconds (millisecond
    /// precision).
    pub fn duration_secs(&self) -> f64 {
        let d = self.duration();
        d.num_milliseconds() as f64 / 1000.0
    }

    /// `true` if [`exit_code`][Self::exit_code] is `0`.
    pub fn is_success(&self) -> bool {
        self.exit_code == 0
    }

    /// Returns the `group`-th capture of the first match of `pattern`
    /// against stdout, or `None` if there's no match (or stdout is
    /// empty).
    ///
    /// `group` is 1-indexed (0 returns the entire match). For named
    /// groups use [`re_search_named`][Self::re_search_named].
    ///
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid regex. Patterns are expected
    /// to be literal strings in source — malformed patterns indicate a
    /// programmer bug, surfaced loudly. If your pattern is sourced at
    /// runtime, validate it with `regex::Regex::new` first.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // From `ping`-style output:
    /// let loss = result.re_search(r"(\d+)% packet loss", 1);
    /// if let Some(loss) = loss {
    ///     println!("loss = {loss}%");
    /// }
    /// ```
    pub fn re_search(&self, pattern: &str, group: usize) -> Option<String> {
        let re = compile_or_panic(pattern);
        let stdout = self.stdout.as_deref()?;
        re.captures(stdout)?
            .get(group)
            .map(|m| m.as_str().to_owned())
    }

    /// Returns the named capture group `name` from the first match of
    /// `pattern` against stdout, or `None` if there's no match.
    ///
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid regex (see
    /// [`re_search`][Self::re_search]).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ip = result.re_search_named(
    ///     r"inet (?P<addr>\d+\.\d+\.\d+\.\d+)",
    ///     "addr",
    /// );
    /// ```
    pub fn re_search_named(&self, pattern: &str, name: &str) -> Option<String> {
        let re = compile_or_panic(pattern);
        let stdout = self.stdout.as_deref()?;
        re.captures(stdout)?
            .name(name)
            .map(|m| m.as_str().to_owned())
    }

    /// Returns all capture groups (indexes 1..N) of the first match
    /// of `pattern` against stdout, or `None` if there's no match.
    ///
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid regex (see
    /// [`re_search`][Self::re_search]).
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Parse min/avg/max from a ping summary:
    /// let groups = result.re_groups(r"min/avg/max = ([\d.]+)/([\d.]+)/([\d.]+)");
    /// if let Some(g) = groups {
    ///     let (min, avg, max) = (&g[0], &g[1], &g[2]);
    ///     // …
    /// }
    /// ```
    pub fn re_groups(&self, pattern: &str) -> Option<Vec<String>> {
        let re = compile_or_panic(pattern);
        let stdout = self.stdout.as_deref()?;
        let caps = re.captures(stdout)?;
        Some(
            (1..caps.len())
                .map(|i| {
                    caps.get(i)
                        .map(|m| m.as_str().to_owned())
                        .unwrap_or_default()
                })
                .collect(),
        )
    }

    /// Returns every non-overlapping match of `pattern` against stdout
    /// as a `Vec<String>`. Empty when there are no matches or stdout
    /// is empty — no `Option` indirection.
    ///
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid regex (see
    /// [`re_search`][Self::re_search]).
    ///
    /// # Example
    ///
    /// ```ignore
    /// // All IPv4 addresses in `ip addr show` output:
    /// let ips = result.re_findall(r"\d+\.\d+\.\d+\.\d+");
    /// ```
    pub fn re_findall(&self, pattern: &str) -> Vec<String> {
        let re = compile_or_panic(pattern);
        let Some(stdout) = self.stdout.as_deref() else {
            return Vec::new();
        };
        re.find_iter(stdout)
            .map(|m| m.as_str().to_owned())
            .collect()
    }
}

fn compile_or_panic(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| panic!("invalid regex {pattern:?}: {e}"))
}

fn strip_cr(s: String) -> String {
    if s.contains('\r') {
        s.replace('\r', "")
    } else {
        s
    }
}

#[derive(Serialize)]
struct ShellResultRepr<'a> {
    command: &'a str,
    stdout: Option<Vec<&'a str>>,
    stderr: Option<Vec<&'a str>>,
    exit_code: i32,
    started: String,
    duration: f64,
}

impl Serialize for ShellResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ShellResultRepr {
            command: &self.command,
            stdout: self.stdout.as_deref().map(|s| s.lines().collect()),
            stderr: self.stderr.as_deref().map(|s| s.lines().collect()),
            exit_code: self.exit_code,
            started: self.started.to_rfc3339(),
            duration: self.duration_secs(),
        }
        .serialize(serializer)
    }
}

impl std::fmt::Display for ShellResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pretty = serde_json::to_string_pretty(self).map_err(|_| std::fmt::Error)?;
        f.write_str(&pretty)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn sample(stdout: Option<&str>, stderr: Option<&str>, exit_code: i32) -> ShellResult {
        let started = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        ShellResult::new(
            "echo hi",
            stdout.map(str::to_owned),
            stderr.map(str::to_owned),
            exit_code,
            started,
        )
    }

    #[test]
    fn stdout_strips_carriage_returns() {
        let r = sample(Some("line one\r\nline two\r\n"), None, 0);
        assert_eq!(r.stdout(), Some("line one\nline two\n"));
    }

    #[test]
    fn stderr_is_returned_verbatim() {
        let r = sample(None, Some("oops"), 1);
        assert_eq!(r.stderr(), Some("oops"));
    }

    #[test]
    fn is_success_tracks_exit_code() {
        assert!(sample(None, None, 0).is_success());
        assert!(!sample(None, None, 1).is_success());
        assert!(!sample(None, None, 127).is_success());
    }

    #[test]
    fn duration_is_non_negative() {
        let r = sample(None, None, 0);
        assert!(r.duration() >= chrono::Duration::zero());
    }

    #[test]
    fn re_search_returns_requested_group() {
        let r = sample(Some("hello world 42"), None, 0);
        assert_eq!(r.re_search(r"(\w+) (\w+) (\d+)", 2), Some("world".into()));
        assert_eq!(r.re_search(r"(\w+) (\w+) (\d+)", 3), Some("42".into()));
    }

    #[test]
    fn re_search_named_group() {
        let r = sample(Some("name=tom age=33"), None, 0);
        assert_eq!(
            r.re_search_named(r"name=(?P<n>\w+)", "n"),
            Some("tom".into())
        );
    }

    #[test]
    fn re_search_returns_none_on_no_match() {
        let r = sample(Some("foo"), None, 0);
        assert_eq!(r.re_search(r"bar", 0), None);
    }

    #[test]
    fn re_search_returns_none_when_stdout_is_none() {
        let r = sample(None, None, 0);
        assert_eq!(r.re_search(r"anything", 0), None);
    }

    #[test]
    fn re_groups_returns_all_captures() {
        let r = sample(Some("a=1 b=2"), None, 0);
        let groups = r.re_groups(r"(\w)=(\d)").unwrap();
        assert_eq!(groups, vec!["a", "1"]);
    }

    #[test]
    fn re_findall_returns_all_matches() {
        let r = sample(Some("1 22 333 4444"), None, 0);
        assert_eq!(r.re_findall(r"\d+"), vec!["1", "22", "333", "4444"]);
    }

    #[test]
    fn re_findall_returns_empty_when_no_matches() {
        let r = sample(Some("abc"), None, 0);
        assert!(r.re_findall(r"\d+").is_empty());
    }

    #[test]
    #[should_panic(expected = "invalid regex")]
    fn invalid_regex_panics() {
        let r = sample(Some("foo"), None, 0);
        let _ = r.re_search(r"(", 0);
    }

    #[test]
    fn display_is_pretty_json() {
        let r = sample(Some("first\nsecond"), Some("warn"), 0);
        let s = format!("{r}");
        assert!(s.contains("\"command\": \"echo hi\""));
        assert!(s.contains("\"first\""));
        assert!(s.contains("\"second\""));
        assert!(s.contains("\"warn\""));
        assert!(s.contains("\"exit_code\": 0"));
    }

    #[test]
    fn json_repr_matches_python_shape() {
        // The JSON shape: command, stdout (list of lines), stderr (list of
        // lines), exit_code, started, duration. Pin it down.
        let r = sample(Some("a\nb"), None, 0);
        let value: serde_json::Value = serde_json::from_str(&format!("{r}")).unwrap();
        assert_eq!(value["command"], "echo hi");
        assert_eq!(value["stdout"], serde_json::json!(["a", "b"]));
        assert_eq!(value["stderr"], serde_json::Value::Null);
        assert_eq!(value["exit_code"], 0);
        assert!(value["started"].is_string());
        assert!(value["duration"].is_number());
    }
}
