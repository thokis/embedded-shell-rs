//! `eshell repl` — interactive line-by-line REPL.
//!
//! Each line is run via the same framed exec protocol the rest of the
//! CLI uses, so stdout, stderr, and the exit code stay cleanly
//! separated. Built-in commands are prefixed with `\`.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use embedded_shell::shell::{Command, ShellError};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::cli::ReplArgs;
use crate::shell::open_linux;

/// Per-line device-side timeout. Bumped well above the library's 5s
/// default because real interactive sessions routinely run
/// `journalctl`, `find /`, slow builds, etc. Overridable per-session
/// with the `\timeout <secs>` built-in.
const DEFAULT_REPL_TIMEOUT_SECS: u64 = 30;

pub async fn run(args: ReplArgs, port: Option<&str>, password: Option<&str>) -> Result<ExitCode> {
    let mut shell = open_linux(port, password).await?;
    let target = port.unwrap_or("local").to_string();
    let use_color = std::io::stdout().is_terminal() && std::io::stderr().is_terminal();

    let mut rl = DefaultEditor::new()?;
    let history_path = if args.no_history {
        None
    } else {
        history_path()
    };
    if let Some(p) = &history_path {
        let _ = rl.load_history(p);
    }

    println!("eshell repl on {target}. \\help for built-ins, \\quit to exit (or Ctrl-D).");

    let mut last_exit: Option<i32> = None;
    let mut timeout_secs: u64 = DEFAULT_REPL_TIMEOUT_SECS;
    loop {
        let prompt = build_prompt(&target, last_exit, use_color);
        let line = match rl.readline(&prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C in input — clear and re-prompt.
                last_exit = None;
                continue;
            }
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("eshell repl: readline: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(trimmed);

        if let Some(rest) = trimmed.strip_prefix('\\') {
            match handle_builtin(rest.trim(), &mut timeout_secs) {
                Builtin::Quit => break,
                Builtin::Continue => {
                    last_exit = None;
                    continue;
                }
            }
        }

        let cmd = Command::new("sh")
            .args(["-c", trimmed])
            .timeout(Duration::from_secs(timeout_secs))
            .allow_nonzero();
        let result = match shell.run(&cmd).await {
            Ok(r) => r,
            Err(
                ShellError::CommandFailed(r)
                | ShellError::CommandNotFound { result: r, .. }
                | ShellError::Timeout { result: r, .. },
            ) => *r,
            Err(e) => {
                eprintln!("eshell repl: {e}");
                last_exit = None;
                continue;
            }
        };

        write_segment(result.stdout(), false, use_color);
        write_segment(result.stderr(), true, use_color);
        last_exit = Some(result.exit_code());
    }

    if let Some(p) = &history_path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = rl.save_history(p);
    }
    let _ = shell.deactivate().await;
    Ok(ExitCode::SUCCESS)
}

enum Builtin {
    Quit,
    Continue,
}

fn handle_builtin(cmd: &str, timeout_secs: &mut u64) -> Builtin {
    match cmd {
        "q" | "quit" | "exit" => return Builtin::Quit,
        "help" | "?" => {
            println!("Built-ins:");
            println!("  \\quit, \\q, \\exit   end the session");
            println!("  \\timeout [N]        show or set per-command timeout (seconds)");
            println!("  \\help, \\?           this message");
            println!();
            println!("Current per-command timeout: {timeout_secs}s");
            println!();
            println!("Anything else is run via `sh -c` on the target.");
            println!("stdout, stderr, and the exit code are framed separately;");
            println!("a non-zero exit code shows in the next prompt.");
            println!();
            println!("Caveats:");
            println!("  - Ctrl-C aborts eshell. The remote command may still finish.");
            println!("  - Each line is independent — `cd /tmp` does not persist.");
            println!("    Use `sh -c 'cd /tmp && …'` to scope a single line.");
            return Builtin::Continue;
        }
        _ => {}
    }
    if let Some(rest) = cmd.strip_prefix("timeout") {
        let rest = rest.trim();
        if rest.is_empty() {
            println!("current per-command timeout: {timeout_secs}s");
        } else {
            match rest.parse::<u64>() {
                Ok(0) => eprintln!("eshell repl: \\timeout must be at least 1 second"),
                Ok(n) => {
                    *timeout_secs = n;
                    println!("per-command timeout set to {n}s");
                }
                Err(_) => eprintln!("eshell repl: \\timeout: expected a number, got `{rest}`"),
            }
        }
        return Builtin::Continue;
    }
    eprintln!("eshell repl: unknown built-in: \\{cmd}  (try \\help)");
    Builtin::Continue
}

fn build_prompt(target: &str, last_exit: Option<i32>, use_color: bool) -> String {
    match last_exit {
        Some(code) if code != 0 => {
            if use_color {
                format!("[\x1b[31m{code}\x1b[0m] {target}> ")
            } else {
                format!("[{code}] {target}> ")
            }
        }
        _ => format!("{target}> "),
    }
}

fn write_segment(seg: Option<&str>, is_stderr: bool, use_color: bool) {
    let Some(s) = seg.filter(|s| !s.is_empty()) else {
        return;
    };
    let needs_newline = !s.ends_with('\n');
    if is_stderr {
        if use_color {
            eprint!("\x1b[31m{s}\x1b[0m");
        } else {
            eprint!("{s}");
        }
        if needs_newline {
            eprintln!();
        }
    } else {
        print!("{s}");
        if needs_newline {
            println!();
        }
    }
}

/// Persistent line-history location. XDG_STATE_HOME if set, otherwise
/// `$HOME/.local/state/eshell/history`. Returns `None` if neither is
/// available (no usable home).
fn history_path() -> Option<PathBuf> {
    let state_dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(state_dir.join("eshell").join("history"))
}
