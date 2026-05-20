# CLAUDE.md

Entry point for any agent or human picking up this codebase.
Deeper rationale lives in `DESIGN.md` (referenced as **D-XXX** throughout).
User-facing docs and event schema are in `README.md`.

## What this crate is

Async driver for Linux and U-Boot devices accessed over a serial line.
A `Shell` trait + concrete shells (`SubprocessShell`, `LinuxSerialShell`,
`UBootSerialShell`) on top of a `SerialTransport` that is generic over any
`AsyncRead + AsyncWrite + Send + Unpin` source. Every command is framed
deterministically with `\x1f` sentinels (Linux) or `RETURNCODE=` (U-Boot)
so output parsing is byte-exact, never prompt-matched.

## Stability

Pre-1.0 (0.x). The following are intended to remain stable across 0.x
releases; everything else may change:

- `Shell` trait method signatures
- `ShellError` variant names
- Event messages and field names listed in `README.md`
- `Command` builder method names (`new`, `arg`, `args`, `timeout`, `cwd`, `allow_nonzero`)

A 1.0 release will commit the full public API.

## Build / test / lint

```sh
scripts/ci.sh                               # full local CI: fmt + build + test + clippy + doc
scripts/ci.sh --test                        # just the test step
scripts/ci.sh --hardware [target]           # opt-in hardware tests (linux | uboot | linux-crate | transfer)
```

Or the raw commands directly:

```sh
cargo build --workspace --all-features
cargo test --workspace --all-features       # 200+ in-process unit tests, no hardware needed
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps --all-features
```

Run a single test:

```sh
cargo test <name>
```

See structured event output during a test:

```sh
RUST_LOG=embedded_shell=debug cargo test -- --nocapture
```

## Hard rules

### Construction

- **Use the builder pattern for shells.** `LinuxSerialShell::builder(port)…open().await?`.
  *Why: validation happens at one point; optional fields stay optional.*
- **Do not add positional `open(port, baudrate, user, pass)` constructors.** They were removed. See **D-004**.
- **Defer regex validation to the terminal method (`.open()`)** and return `ShellError::InvalidRegex`. See **D-007**.
- **Builder setters are infallible.** They store raw values; `.open()` validates everything in one shot.

### Commands

- **Argv-only `Command`.** `Command::new("ls").arg("-la").arg(path)`.
- **Pipes / redirects / env-prefix via explicit `sh -c`.**
  `Command::new("sh").args(["-c", "cmd | other"])`. See **D-003**.
- **There is no `Command::shell(raw)` constructor.** Do not add one back.

### Errors

- **Use named variants of `ShellError`**, never generic strings.
  Variants: `CommandFailed`, `CommandNotFound`, `Timeout`, `ReadTimeout`,
  `InvalidRegex`, `Initialization`, `Io`. See **D-006**.

### Logging

- **The library uses `tracing`**, never `log`.
- **Never install a tracing subscriber from inside the library.** The
  consumer's binary configures output. See **D-012**.
- **Event messages and field names listed in README.md are public API.** See **D-013**.

### Reconnect

- **No auto-reconnect inside `run()`.** Disconnects surface as
  `ShellError::Io`; the caller composes the policy. See **D-011**.
- **Use `reconnect()`**, which performs port reopen + activate in one
  call. Do not introduce a `reopen` method that requires a separate
  follow-up `activate()`.

### Crate framing

- **The crate stands on its own.** No references in source comments, doc
  comments, or README to predecessor projects, inspiration sources, or
  specific downstream consumers. See **D-014**.

## Conventions

- **Tests live in `#[cfg(test)] mod tests`** inside source files (no
  separate `tests/` directory for unit tests).
- **Test-only constructors are gated with `#[cfg(test)] pub(crate)`** —
  `from_transport(...)` is the canonical name. Production code goes
  through the builder only.
- **All transport tests use `tokio::io::duplex(8192)`** for synthetic
  byte streams. Real hardware is never required for CI. See **D-008**.
- **Mark recommendations explicitly** when offering choices.
- **Test-injected ports for `reconnect()`**: a test shell built via
  `from_transport(...)` has an empty `port` string and `reconnect()`
  returns `ShellError::Initialization` rather than attempting to open
  `""`. Production shells always have a real port.

## Anti-patterns explicitly caught and avoided

These all came up during initial development. Don't re-introduce them.

- **`try_X` + `X` dual-API smell.** We removed `try_re_search` because
  `re_search` already covers the use case via `Option<String>`. One path
  per operation. See **D-005**.
- **Panic in builder setters.** Setters that panic mid-chain on bad input
  are an anti-pattern. Setters are infallible; the terminal method returns
  `Result`. See **D-007**.
- **Reopen-without-activate.** A method that puts the shell in a state
  where it can't run commands is useless ceremony. `reconnect()` does
  the whole thing. See **D-011**.
- **Dual-mode types with runtime panic.** `Command::shell(raw).arg(...)`
  used to panic at runtime. We removed `Command::shell` rather than keep
  a method that compiles but panics for some receivers. See **D-003**.
- **Speculative trait bounds.** `Shell: Send + Sync` was dropped because
  `Sync` was not actually needed — a stateful shell behind `&mut self`
  doesn't get shared without an explicit `Mutex`. Add bounds only when
  a concrete need appears.

## File map

This is the `embedded-shell-rs` workspace, hosting the generic
`embedded-shell-*` family of crates. **Domain-specific consumers live in
their own repositories** and depend on these crates via crates.io or git
refs. They are NOT in this workspace.

```
embedded-shell-rs/                    ← workspace root
├── Cargo.toml                         ← [workspace] manifest
├── README.md                          ← user-facing docs + event schema
├── CLAUDE.md                          ← this file
├── DESIGN.md                          ← decisions (D-000…D-100)
└── crates/
    ├── embedded-shell/                ← the foundation: Shell trait + concrete shells
    │   ├── Cargo.toml
    │   ├── examples/
    │   │   └── init_logging.rs        ← tracing + EnvFilter + journald
    │   ├── tests/
    │   │   ├── common/mod.rs          ← shared open_at_linux/open_at_uboot probes
    │   │   ├── hardware_linux.rs      ← #[ignore]d Linux-shell tests
    │   │   └── hardware_uboot.rs      ← #[ignore]d U-Boot-shell tests
    │   └── src/
    │       ├── lib.rs                 ← crate root, re-exports
    │       ├── test_utils.rs          ← `test-utils` feature: state-aware probes
    │       └── shell/
    │           ├── mod.rs             ← module exports
    │           ├── command.rs         ← Command builder, POSIX quoting
    │           ├── error.rs           ← ShellError enum
    │           ├── linux.rs           ← LinuxSerialShell + builder
    │           ├── prompts.rs         ← prompt detection + PromptDetector
    │           ├── result.rs          ← ShellResult + regex helpers
    │           ├── serial.rs          ← SerialTransport (pub(crate))
    │           ├── subprocess.rs      ← SubprocessShell (local sh -c)
    │           ├── traits.rs          ← Shell, LinuxShell traits
    │           └── uboot.rs           ← UBootSerialShell + builder
    ├── embedded-shell-linux/          ← thin wrappers around Linux userland CLI tools
    │   ├── Cargo.toml
    │   ├── tests/
    │   │   └── hardware.rs            ← #[ignore]d hardware integration tests
    │   └── src/
    │       ├── lib.rs                 ← crate root, feature-gated module exports
    │       ├── error.rs               ← Error + Result
    │       ├── fs.rs                  ← read/write/copy/rename/symlink/walk_dir/metadata/sha256sum/…
    │       ├── iputils.rs             ← ping + arping (default-on feature)
    │       ├── systemd.rs             ← systemctl: is_active/start/stop/restart/enable/… + UnitStatus
    │       ├── journalctl.rs          ← tail / tail_unit, structured LogEntry
    │       ├── iproute2.rs            ← links/addresses/routes via `ip -j` JSON
    │       ├── networkmanager.rs      ← connections + devices via `nmcli -t`
    │       └── modemmanager.rs        ← list_modems + modem(index) via `mmcli -J`
    ├── embedded-shell-transfer/       ← file push/fetch, multi-transport
    │   ├── Cargo.toml
    │   ├── tests/
    │   │   └── hardware.rs
    │   └── src/
    │       ├── lib.rs                 ← crate root, event-schema docs
    │       ├── error.rs               ← TransferError + Result
    │       ├── http.rs                ← hyper server on host + wget/curl on device
    │       └── serial.rs              ← base64 over the shell line
    └── embedded-shell-cli/            ← CLI built on top of the three libraries (binary: `eshell`)
        ├── Cargo.toml
        ├── README.md
        └── src/
            ├── main.rs                ← tracing init + subcommand dispatch
            ├── cli.rs                 ← clap-derive subcommand structs
            ├── shell.rs               ← shared open_linux() helper
            └── commands/              ← one file per subcommand
                ├── mod.rs
                ├── exec.rs
                ├── push.rs            ← HTTP-first with serial fallback
                ├── pull.rs            ← HTTP-first with serial fallback
                ├── info.rs            ← sectioned summary (System/Storage/Interfaces)
                ├── ping.rs
                ├── reboot.rs
                ├── service.rs         ← single systemd unit (status / start / …)
                ├── services.rs        ← units grouped by active state (`Failed`/`Active`/…)
                ├── journal.rs         ← `journalctl` tail / since / unit
                ├── modem.rs           ← `mmcli` modem + SIM details
                ├── network.rs         ← combined iproute2 + NetworkManager view
                ├── repl.rs            ← rustyline-backed interactive REPL
                ├── completions.rs    ← shell-completion script generation
                ├── devices.rs        ← list /dev/ttyUSB*+ACM* with USB descriptors
                └── cat.rs            ← read files (text + binary base64) from the device
```

## Planned work in this workspace

- **MicroPython shell backend.** A second concrete `Shell` impl (next
  to `LinuxSerialShell` / `UBootSerialShell` / `SubprocessShell`),
  driving MicroPython's raw REPL mode. Strategic milestone proving the
  trait abstraction works across genuinely different shell families.
- **`embedded-shell-uboot` crate** (speculative): U-Boot-specific
  wrappers (`printenv`, `setenv`, `tftpboot`, `loady`/`loadb`) behind a
  `UBootShell` marker trait, mirroring the `LinuxShell` pattern.
- **CLI output polish across remaining commands.** Done for `info`,
  `services`, `journal`, `network`, and `modem`. Future polish lands
  in command-specific sub-flags (eg. `--follow` for `journal`).
- **`fs::read_to_string` loses trailing newlines.** The framing
  wrapper's `$(cat /tmp/out)` command substitution strips them
  (POSIX rule). `eshell cat` papers over this by appending a `\n`
  if missing; library callers see the bare bug. A library-level fix
  would either teach the wrapper to preserve trailing bytes, or
  switch `read_to_string` to the base64 path that `read` already
  uses (byte-faithful but slower).
- **Library: cancellation on `Shell::run`.** REPL races `run()`
  against `tokio::signal::ctrl_c()` and drops the future on Ctrl-C
  to exit gracefully; the device-side command may still finish. A
  library-level fix would pass a `tokio_util::sync::CancellationToken`
  into `run()` and have the framed-exec layer send `\x03` plus drain
  to the next prompt — properly aborts the device-side command and
  leaves the transport in a known-good state.
- **Publishing prep.** Cargo.toml metadata audit, `CHANGELOG.md`,
  `cargo publish --dry-run` per crate to surface blockers before 0.1.
- **Polish across existing wrapper modules**: `networkmanager::wifi_list`,
  `modemmanager::sim` (SIM details). Each small.

## What does *not* live in this repo

- **Domain-specific code** (e.g. device-family identifiers, EEPROM
  layouts, vendor-specific password derivation, brand-named applications):
  lives in separate repositories that depend on these crates from
  crates.io or git. Don't add domain-specific code to this workspace.
