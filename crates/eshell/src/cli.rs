//! `clap`-derive definitions for `eshell`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "eshell",
    version,
    about = "Command-line driver for embedded-shell-rs devices over a serial line."
)]
pub struct Cli {
    /// Login password if the device requires one. Reads `ESHELL_PASSWORD`
    /// from the environment when the flag is absent.
    #[arg(long, env = "ESHELL_PASSWORD", global = true, hide_env_values = true)]
    pub password: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Execute one command on the device.
    Exec(ExecArgs),
    /// Push a local file to the device. Defaults to HTTP, falls back
    /// to serial automatically when HTTP isn't available.
    Push(PushArgs),
    /// Pull a file from the device to the local host. Same fallback
    /// policy as `push`.
    Pull(PullArgs),
    /// Print a one-page device summary.
    Info(InfoArgs),
    /// Ping a target from the device.
    Ping(PingArgs),
    /// Reboot the device and wait for it to come back.
    Reboot(RebootArgs),
    /// Inspect or control one systemd unit.
    Service(ServiceArgs),
    /// Tail the systemd journal.
    Journal(JournalArgs),
    /// Show details of one cellular modem (and its primary SIM).
    Modem(ModemArgs),
}

/// Common arguments accepted by every subcommand.
#[derive(Args, Clone)]
pub struct Common {
    /// Serial port the device is on (e.g. `/dev/ttyUSB0`).
    pub port: String,
}

#[derive(Args)]
pub struct ExecArgs {
    #[command(flatten)]
    pub common: Common,
    /// Command and arguments to run on the device. Everything after `--`
    /// is forwarded verbatim.
    #[arg(trailing_var_arg = true, required = true, allow_hyphen_values = true)]
    pub argv: Vec<String>,
    /// Emit `{stdout, stderr, exit_code, duration_ms}` as JSON instead
    /// of streaming stdout/stderr separately.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct PushArgs {
    #[command(flatten)]
    pub common: Common,
    /// Local source path.
    #[arg(long)]
    pub src: PathBuf,
    /// Remote destination path on the device.
    #[arg(long)]
    pub dst: String,
    /// Permission bits to set on the destination after a successful
    /// transfer (e.g. `0644`). Skipped if absent.
    #[arg(long)]
    pub mode: Option<String>,
    /// Force a specific transport. Default is HTTP-first with serial
    /// fallback.
    #[arg(long, value_enum)]
    pub via: Option<Transport>,
}

#[derive(Args)]
pub struct PullArgs {
    #[command(flatten)]
    pub common: Common,
    /// Remote source path on the device.
    #[arg(long)]
    pub src: String,
    /// Local destination. If it's a directory, the basename of `src`
    /// is appended.
    #[arg(long)]
    pub dst: PathBuf,
    /// Force a specific transport. Default is HTTP-first with serial
    /// fallback.
    #[arg(long, value_enum)]
    pub via: Option<Transport>,
}

#[derive(Args)]
pub struct InfoArgs {
    #[command(flatten)]
    pub common: Common,
    /// Emit the result as JSON on stdout.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct PingArgs {
    #[command(flatten)]
    pub common: Common,
    /// Target hostname or IP — passed verbatim to the device's `ping`.
    pub target: String,
    /// Number of ICMP echo requests to send.
    #[arg(long, default_value_t = 4)]
    pub count: u32,
    /// Emit `{target, transmitted, received, loss_percent, rtt_*}` as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct RebootArgs {
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args)]
pub struct ServiceArgs {
    #[command(flatten)]
    pub common: Common,
    /// systemd unit name, e.g. `sshd.service` (the `.service` suffix
    /// is optional; systemd infers it).
    pub unit: String,
    /// What to do with the unit. `status` prints the structured
    /// state; `start`/`stop`/`restart`/`reload` perform that action;
    /// `enable`/`disable` toggle boot persistence (and never start
    /// or stop the unit themselves).
    #[arg(value_enum)]
    pub action: ServiceAction,
    /// For `status`: emit the `UnitStatus` as JSON instead of the
    /// human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ServiceAction {
    Status,
    Start,
    Stop,
    Restart,
    Reload,
    Enable,
    Disable,
}

#[derive(Args)]
pub struct JournalArgs {
    #[command(flatten)]
    pub common: Common,
    /// Filter to one systemd unit. Without this, every accessible
    /// source is included.
    #[arg(long)]
    pub unit: Option<String>,
    /// Time-window filter. Passed verbatim to `journalctl --since`
    /// — accepts everything it does: `"1 hour ago"`, `"yesterday"`,
    /// `"2024-01-15"`, …
    #[arg(long)]
    pub since: Option<String>,
    /// Number of recent entries to return (only honored when
    /// `--since` isn't set).
    #[arg(short = 'n', long, default_value_t = 50)]
    pub count: u32,
    /// Emit each entry as JSON on its own line (JSONL) instead of
    /// human-readable text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ModemArgs {
    #[command(flatten)]
    pub common: Common,
    /// Modem index from `mmcli -L`. Default 0 — fine on the typical
    /// single-modem device.
    #[arg(default_value_t = 0)]
    pub index: u32,
    /// Skip the SIM lookup (faster, and avoids erroring when no
    /// SIM is inserted).
    #[arg(long)]
    pub no_sim: bool,
    /// Emit the modem (and SIM) details as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Transport {
    /// HTTP server on the host + `wget`/`curl` on the device.
    Http,
    /// Base64 over the shell line — works without a network.
    Serial,
}
