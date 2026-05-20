# DESIGN.md

A flat list of design decisions, ADR-style. Each section is self-contained
so you can jump to one without reading the others. Cross-references use the
**D-NNN** identifier.

For day-to-day rules and conventions, see `CLAUDE.md`.
For user-facing documentation, see `README.md`.

---

## D-000 — Why this crate exists alongside labgrid / pexpect / rexpect

**Decision:** Build a focused async Rust crate for driving Linux + U-Boot
devices over serial, rather than fit workflows into existing tools.

**Context:** There are existing libraries in this space:

- `labgrid` (Python) — declarative resource/strategy model, sync,
  pytest-integrated, optimised for fleets in a CI lab
- `pexpect` (Python), `rexpect` (Rust), `expectrl` (Rust) — prompt-matching
  expect-style libraries
- `paramiko` / `fabric` — SSH-based, not serial
- `serialport-rs` / `tokio-serial` — raw transport, no shell semantics

None of them combine async-first imperative scripting, deterministic exec
framing, the same `Shell` trait for local/Linux/U-Boot, and configurable
prompts behind a builder API.

**Alternatives considered:**

- Adopt `labgrid` and write a Python wrapper. Rejected: imperative async
  scripting (`async with shell as s: result = await s.exec(...)`) doesn't
  fit labgrid's resource/strategy model. Also forces Python everywhere.
- Use `rexpect`. Rejected: prompt-matching is fundamentally ambiguous when
  command output contains prompt-shaped strings; we wanted byte-exact
  framing instead.
- Use `serialport-rs` directly and roll the shell logic per-application.
  Rejected: every application would reinvent the framing protocol, prompt
  detection, and login state machine.

**Consequences:**

- The crate has a distinct point in the design space; not redundant with
  any of the above.
- The `\x1f` framing (see **D-001**) is the engineering centrepiece.

---

## D-001 — The `\x1f` exec framing protocol

**Decision:** Wrap every device-side command with framing that uses three
`\x1f` (US, ASCII 0x1f) sentinels, so stdout, stderr, and exit code can be
read back deterministically:

```
(timeout <secs> <cmd>) > /tmp/out 2> /tmp/err; \
    echo -e "$(echo $?)\x1f\n$(cat /tmp/out)\x1f\n$(cat /tmp/err)\x1f"
```

After sending, the host reads bytes until it has seen exactly **three**
`\x1f` bytes. The captured response is split on `\x1f` into exit-code,
stdout, and stderr segments.

**Context:** Most expect-style libraries detect command completion by
matching the next shell prompt. That's ambiguous when command output
contains prompt-shaped substrings (a script that prints something ending
in `#`, output containing `$`, etc.).

**Alternatives considered:**

- **Prompt-matching** (`pexpect`, `labgrid`, `rexpect`). Rejected: false
  positives and false negatives on real command output.
- **Newline-based framing** (use `\n` as the delimiter). Rejected: command
  output naturally contains newlines.
- **JSON-RPC over the serial line.** Rejected: requires device-side
  tooling beyond a stock `bash` / `busybox`.
- **Use a uniquely-named bash variable**, e.g. `__END_a8c4__`.
  Rejected: more brittle than ASCII control bytes that are designed
  precisely for this purpose. (The US byte was named "Unit Separator"
  in ASCII X3.4-1967 for exactly this kind of record-framing job.)

**Consequences:**

- Linux shell uses `\x1f` (Linux's `echo -e` interprets the escape).
- U-Boot uses a different framing (`RETURNCODE=$?`, see **D-001a** below)
  because U-Boot's `echo` typically lacks `-e` and the byte
  representation.
- The `\x1f` byte itself is filtered out of the `console_buffer()` view so
  the user-facing transcript stays readable.
- Implemented in `src/shell/linux.rs` (the `exec_framed` method and the
  `triple_sentinel` / `parse_framed_response` helpers).

---

## D-001a — U-Boot framing variant (`RETURNCODE=`)

**Decision:** For U-Boot, frame commands as `<cmd>; echo RETURNCODE=$?` and
read until a line-anchored `^RETURNCODE=<n>` match.

**Context:** U-Boot's `echo` is minimal; the `-e` flag and `\x1f` byte
interpretation are not portable across U-Boot builds. We need a framing
form that uses only what every U-Boot ships.

**Alternatives considered:**

- Use `\x1f` framing if available, fall back if not. Rejected: adds
  detection logic and two code paths.
- Encode the exit code as a longer marker (e.g. `__UBOOT_RC=0`). Rejected:
  more brittle than `RETURNCODE=` with a line-anchored regex.

**Consequences:**

- U-Boot's framing has no separate stderr (U-Boot doesn't split it).
- Line-anchored regex (`(?m)^RETURNCODE=...`) prevents false positives
  from command output that contains the literal text `RETURNCODE=`.
- Implemented in `src/shell/uboot.rs` (`uboot_returncode_end` and
  `parse_uboot_response`).

---

## D-002 — Strict "one way to do each thing" policy

**Decision:** Across the crate's public API, never provide two paths that
accomplish the same thing. Where alternative paths would offer ergonomic
flexibility, pick one and remove the other.

**Context:** During initial development several attempts to "offer both
styles" produced runtime smells (panicking branches, dual-API confusion,
unclear winners). The user explicitly preferred the maximally-strict
shape.

**Alternatives considered:**

- "Have both" stance — let callers pick the style they prefer. Rejected:
  reviewers six months from now have to ask why each call site picked one
  variant over the other. Sets up bike-shedding.
- Provide one path with a deprecation pipeline. Rejected: pre-1.0; we can
  remove cleanly.

**Consequences:**

- One construction path per shell: the builder. See **D-004**.
- One call form for `run`: `shell.run(&command)`. There's no
  `Command::run(&mut shell)` method.
- One regex helper API on `ShellResult`: `Option<String>`-returning,
  panic on bad regex literal. No `try_re_search` variant. See **D-005**.
- One `Command` representation: argv. No `Command::shell(raw)`. See **D-003**.

---

## D-003 — Argv-only `Command` + explicit `sh -c` for shell features

**Decision:** `Command` stores a `Vec<String>` of argv tokens. Shell
features (pipes, redirects, env-var prefixes, `&&`) are not part of the
`Command` type; callers opt in explicitly with
`Command::new("sh").args(["-c", "..."])`.

**Context:** Earlier iterations had both an argv mode and a `shell(raw)`
mode behind one type, with `.arg()` panicking on shell-mode instances.
That compiled-but-panicked design is exactly the kind of "two ways to do
things" that **D-002** outlaws.

**Alternatives considered:**

- Two distinct types (`Command` for argv, `Script` for shell strings)
  with a sealed `Runnable` trait. Cleaner than the dual-mode type, but
  still two top-level types for callers to choose between, and `Shell::run`
  takes `&dyn Runnable` for dyn dispatch overhead. See the prior design
  draft in source history.
- Typestate `Command<Argv>` / `Command<Shell>`. Adds GAT machinery and
  confusing error messages with our existing `Shell` trait.

**Consequences:**

- POSIX quoting happens once in `command::posix_quote`. Callers can't
  accidentally roll their own.
- Spawning a sub-shell on the device for piped commands costs one extra
  `fork`. Negligible vs serial round-trip latency.
- Implemented in `src/shell/command.rs`.

---

## D-004 — Builder pattern for shell construction

**Decision:** Constructing a `LinuxSerialShell` or `UBootSerialShell` goes
through a builder:

```
LinuxSerialShell::builder("/dev/ttyUSB0")
    .password("…")              // optional
    .login_timeout(Duration::…) // optional
    .open()
    .await?
```

**Context:** The original API had `LinuxSerialShell::open(port, baudrate,
username, password: Option<String>)` plus `with_*` post-construction
setters. That forced every caller to write `None` or `Some(...)` for
optional fields, and offered two ways to configure the shell (positional
constructor + setters), violating **D-002**.

**Alternatives considered:**

- Positional `open(...)` with all defaults inlined. Rejected: optional
  fields like `password` look like first-class concerns of every caller.
- `&Config` struct passed to `open(&config)`. Rejected: configs that grow
  fields are still breaking changes when adding required fields, and the
  syntax is heavier (`Config { port: ..., baudrate: 115200, ..Default::default() }`).
- Typestate builder enforcing required fields. Rejected: only one
  field is required (`port`), and it's taken via `builder(port)`. No
  typestate gymnastics needed.

**Consequences:**

- Defaults: baudrate 115200, username `"root"`, no password, 120s login
  timeout, 1 MiB console buffer cap.
- The `Command` builder predates this and uses the same consuming-self
  pattern.
- Builder setters are infallible; validation happens in `.open()`. See **D-007**.
- Implemented in `src/shell/linux.rs` (`LinuxSerialShellBuilder`) and
  `src/shell/uboot.rs` (`UBootSerialShellBuilder`).

---

## D-005 — No `try_X` duplicates of `X` for the same operation

**Decision:** The regex helpers on `ShellResult` (`re_search`,
`re_search_named`, `re_groups`, `re_findall`) return `Option<...>` for
the no-match case. They panic on invalid regex (programmer bug). There
is no `try_re_search` variant.

**Context:** An earlier iteration provided both `re_search -> Option` and
`try_re_search -> Result<String, ShellError>`, the latter erroring on
no-match. This was a code smell: same operation, two error encodings,
caller picks by style preference. Doesn't match any std-lib precedent —
`str::find`, `HashMap::get`, `String::strip_prefix` all return `Option`.

**Alternatives considered:**

- Keep both. Rejected: violates **D-002**.
- Return `Result<String, ShellError>` for every helper. Rejected: forces
  `?`-propagation on no-match, awkward when no-match is the expected
  branch.
- Return `Result<Option<String>, RegexError>` from every helper. Rejected:
  excessive verbosity for the common "did the regex match" case.

**Consequences:**

- Callers who want strict "no-match = error" semantics write
  `result.re_search(...).ok_or(MyError)?` at the call site.
- `ShellError::RegexNoMatch` and `ShellError::InvalidRegex` variants —
  the latter is still used by the builder's regex validation
  (see **D-007**).
- Implemented in `src/shell/result.rs`.

---

## D-006 — Typed error variants instead of generic strings

**Decision:** `ShellError` is an enum with named, structured variants.
No `ShellError::Other(String)` catch-all.

```rust
enum ShellError {
    Initialization(String),
    CommandFailed(Box<ShellResult>),
    CommandNotFound { command: String, result: Box<ShellResult> },
    Timeout { duration: Duration, result: Box<ShellResult> },
    ReadTimeout { duration: Duration, captured: Vec<u8> },
    InvalidRegex { pattern: String, source: regex::Error },
    Io(io::Error),
}
```

**Context:** The Python source library used an exception class with a
message string + a side-bolted `result` attribute. Callers had to inspect
strings to distinguish error kinds.

**Alternatives considered:**

- One `ShellError(String)` newtype. Rejected: forces string-matching for
  error handling — exactly the smell we're moving away from.
- Per-shell error types (`LinuxShellError`, `UBootShellError`). Rejected:
  most variants apply to both; unification simplifies caller code.
- Use `anyhow::Error` directly. Rejected: libraries should expose typed
  errors so consumers can pattern-match.

**Consequences:**

- Callers do `match err { ShellError::CommandFailed(r) => ... }`, not
  string parsing.
- Adding a new variant is a minor-version-compatible addition; renaming
  one is a major-version break (see Stability in `CLAUDE.md`).
- `#[from]` on `Io(io::Error)` lets `?` convert seamlessly.
- Implemented in `src/shell/error.rs`.

---

## D-007 — Defer regex validation to the terminal builder method

**Decision:** Builder setters for regex patterns (`shell_prompt`,
`login_prompt`) store the raw `String` and never panic. Validation
happens in `.open()`, which returns `Result<_, ShellError::InvalidRegex>`
when a pattern is malformed.

**Context:** An earlier iteration had setters that panicked on invalid
regex. Patterns may come from config files, CLI arguments, or env vars —
not just literal strings in source — so a programmer-bug panic was
the wrong model.

**Alternatives considered:**

- Setters return `Result<Self, ShellError>`. Rejected: forces `?` in the
  middle of a fluent chain. Workable but ugly.
- Validate in setters; panic on bad pattern. Rejected: doesn't suit
  patterns sourced from config/runtime.
- Validate at first use during `activate()`. Rejected: error happens too
  far from configuration site; harder to attribute.

**Consequences:**

- `.open()` is the single point where regex compilation can fail.
- The asymmetry with `re_search` (which panics on invalid regex) is
  intentional: `re_search` patterns are always literal in caller code,
  while builder patterns may be runtime-sourced.
- `ShellError::InvalidRegex { pattern, source }` carries both the
  offending pattern and the underlying `regex::Error`.
- Implemented in both `src/shell/linux.rs` and `src/shell/uboot.rs`
  via the shared `prompts::PromptDetector::try_compile`.

---

## D-008 — Transport generic over `AsyncRead + AsyncWrite` for testing

**Decision:** `SerialTransport::new<T>(io: T) where T: AsyncRead +
AsyncWrite + Send + Unpin + 'static`. The reader half is moved into a
background task; the writer half is type-erased as
`Box<dyn AsyncWrite + Send + Unpin>` and stored on the struct.

**Context:** Production opens a `tokio_serial::SerialStream`. Tests use
`tokio::io::duplex(8192)` — an in-memory bidirectional async pipe — so the
same code paths run in CI without hardware.

**Alternatives considered:**

- `SerialTransport<T>` generic struct. Rejected: makes every caller of
  `SerialTransport` generic, including `LinuxSerialShell` and
  `UBootSerialShell`. Two layers of generics for no real benefit.
- Concrete `SerialStream` only. Rejected: every test would need
  `/dev/pts/*` or a real device. CI becomes hardware-coupled.
- Custom in-house mock socket. Rejected: `tokio::io::duplex` already
  exists.

**Consequences:**

- Tests for `LinuxSerialShell` and `UBootSerialShell` exercise the full
  state machine including login flow, framing, and prompt detection,
  all in-process. 106 tests run in ~1 second.
- One `Box<dyn ...>` indirection per write/read call. Irrelevant at
  serial baud rates.
- Implemented in `src/shell/serial.rs`.

---

## D-009 — `timeout(1)` wrapper, not host-side `tokio::time::timeout`

**Decision:** Every command run on a Linux device is wrapped in
`timeout(1)` device-side: `timeout <secs> <cmd>`. We do not use
`tokio::time::timeout` to enforce command deadlines on the host side.

**Context:** A serial line is a one-way control channel — the host cannot
truly kill a remote process. If we relied on host-side timeout, a
runaway command would keep consuming the device's shell beyond our
deadline, polluting the next command's output.

**Alternatives considered:**

- `tokio::time::timeout` only. Rejected for the reason above.
- Both, layered. Rejected: redundant. Host-side timeout would have to fire
  later than device-side to allow `timeout(1)` to do its job, at which
  point it's just a paranoid safety net.
- No timeout, rely on heuristics. Rejected: any hung command stalls all
  future work on the shell.

**Consequences:**

- Linux shell exit code 124 maps to `ShellError::Timeout`.
- U-Boot has no equivalent device-side `timeout` binary. U-Boot shell
  timeouts are host-side only and a hung U-Boot command persists until
  the device is reset. Documented in `src/shell/uboot.rs` and
  `README.md`.
- `SubprocessShell` also uses `timeout(1)` for consistency, despite
  having other options locally.
- Default per-command timeout: 5 seconds (`DEFAULT_EXEC_TIMEOUT`).

---

## D-010 — Console buffer is a bounded ring (default 1 MiB)

**Decision:** `SerialTransport` maintains a persistent transcript of all
bytes received from the port (`Arc<Mutex<Vec<u8>>>`), capped at a
configurable size (default 1 MiB). When the buffer exceeds the cap,
oldest bytes are FIFO-trimmed.

**Context:** Unbounded growth would leak memory on long-running
sessions. A purely fixed buffer that drops new bytes wouldn't preserve
the recent transcript (which is what users want to inspect after a
failure).

**Alternatives considered:**

- Unbounded `Vec<u8>`. Rejected: slow leak; multi-hour shell sessions
  accumulate tens of MB.
- Drop-newest on overflow. Rejected: the recent transcript is the
  diagnostic-valuable part.
- `VecDeque<u8>` ring. Same effective behaviour as `Vec` + FIFO trim;
  `drain(..n)` is O(n + remaining) and amortises well for the
  large-cap / small-chunk case we have.

**Consequences:**

- Default cap of 1 MiB covers verbose boot logs (~50 KB) with two orders
  of magnitude of headroom.
- Configurable per-shell via `.console_buffer_cap(bytes)` on the builder.
- Atomic `usize` for the cap lets users adjust it at runtime; the reader
  task picks up the new value on the next chunk.
- Implemented in `src/shell/serial.rs`.

---

## D-011 — No auto-reconnect; explicit `reconnect()` only

**Decision:** `Shell::run` does not transparently reconnect on transport
errors. Disconnects surface as `ShellError::Io`. To recover, the caller
explicitly invokes `shell.reconnect().await?`, which closes the dead
port, opens a fresh one, and runs `activate()` — leaving the shell
ready to accept commands.

**Context:** Stateful protocols (sessions, prompts, login state) make
auto-reconnect semantically ambiguous: after a mid-command disconnect, we
don't know if the command committed on the device side. A hidden retry
would silently mask whether the original operation succeeded.

**Alternatives considered:**

- Auto-reconnect with backoff inside `run`. Rejected: hides state loss;
  caller can't tell whether the result they got is from attempt 1 or 5.
- Separate `reopen()` + caller-driven `activate()`. Rejected (this was
  the first iteration of the design): `reopen` alone is useless — a
  freshly-opened port without `activate` can't run anything. Ceremony
  without value. We fused them into `reconnect()`.
- Optional auto-reconnect via a builder flag. Considered for a future
  release once the use cases are clearer. Skipped for now to avoid
  carrying complexity speculatively.

**Consequences:**

- Recommended caller pattern is explicit and short:
  ```rust
  match shell.run(&cmd).await {
      Err(ShellError::Io(_)) => {
          shell.reconnect().await?;
          shell.run(&cmd).await
      }
      other => other,
  }
  ```
- The shell remembers `port: String` and `baudrate: u32` for `reconnect`.
  Test shells built via `from_transport` store empty/zero defaults and
  `reconnect()` returns a clear `ShellError::Initialization` rather than
  attempting to open an empty path.
- Implemented in `src/shell/linux.rs` and `src/shell/uboot.rs`.

---

## D-012 — Library uses `tracing`, never installs subscribers

**Decision:** The library emits events via `tracing::{trace, debug, info,
warn, error}!` macros. It does not install a `tracing::Subscriber`,
ever. The consumer's binary configures output.

**Context:** Libraries that install subscribers force every consumer to
accept their choice of output. Different deployment targets need
different sinks (stderr for CLIs, journald for systemd services,
JSON-to-stdout for log-aggregator containers).

**Alternatives considered:**

- Use `log` (the older facade) + `env_logger`. Rejected: `tracing` is
  the modern successor, supports structured fields and spans which we
  use, and `tracing-subscriber::EnvFilter` honours `RUST_LOG` the same
  way `env_logger` does.
- Provide an `embedded_shell::init_logging()` convenience. Rejected:
  prescribes output to all consumers; doesn't compose with binaries
  that already configure their own subscriber.

**Consequences:**

- `examples/init_logging.rs` shows the recommended setup combining
  `EnvFilter` + stderr layer + optional `tracing-journald` layer.
- The library does not depend on `tracing-subscriber` (it's a
  `dev-dependency` only).
- Structured fields like `port = "/dev/ttyUSB0"` are auto-promoted to
  journald fields when `tracing-journald` is used downstream, enabling
  `journalctl PORT=/dev/ttyUSB0` queries.

---

## D-013 — Event schema as public API with stability promise

**Decision:** The set of event messages and their structured field names
is part of the public API. Event messages and field names will not
change in a non-major version bump. New events and new fields on
existing events are minor-version compatible.

The full schema is enumerated in `README.md` under "Event schema".

**Context:** Structured logging is only useful if consumers can rely on
field names being stable. A `journalctl PORT=...` query breaks silently
if we rename `port` to `port_path`. Without a stability commitment the
schema is effectively private.

**Alternatives considered:**

- No commitment; document schema for current version only. Rejected:
  every minor release becomes a journald-query-breaking change for
  consumers.
- Make schema discoverable programmatically via field-name constants in
  a `mod fields`. Rejected: over-engineering for our small surface.
  Documentation in `README.md` is sufficient.
- Stability only for `info` and above. Rejected: trace/debug fields are
  also worth pinning — operators writing dashboards use them.

**Consequences:**

- README.md "Event schema" section is normative; the source-code
  format-strings must match what's documented.
- Adding a new field to an existing event is allowed (consumers using
  the old field continue to see it).
- Removing a field, renaming a field, or changing a message string is a
  major-version break.

---

## D-014 — Crate stands on its own; no references to predecessor projects or specific consumers

**Decision:** Source comments, doc comments, README, and Cargo.toml
description never reference predecessor projects (libraries that
inspired this one), specific downstream consumers, brand names, or
device families. The crate's documentation describes what the crate
*is and does*, not its provenance or who uses it.

**Context:** Carrying "port of X" framing or naming specific consumers
(`brand-Y helpers depend on this`) ties the public face of the crate to
context that ages badly: predecessor projects evolve independently;
named consumers may change or disappear; brand-specific examples narrow
the perceived audience.

**Alternatives considered:**

- Mention predecessor / inspiration in DESIGN.md only. Rejected: even
  there it adds noise without changing the design. The decisions stand
  on their own merits.
- Name specific downstream consumers as examples ("e.g. used by X").
  Rejected: examples drift; today's example is tomorrow's footnote.

**Consequences:**

- Doc comments describe behaviour, not provenance.
- README.md introduces the crate by what it does, not where it came from.
- Tests use generic placeholder hostnames (`device`, not vendor-specific
  names).
- Environment variable prefixes for tests use `EMBEDDED_SHELL_` (not a
  brand or device-family prefix).

---

## D-015 — Generic-only workspace; domain-specific crates live in separate repositories

**Decision:** This repository (`embedded-shell-rs`) hosts the generic
`embedded-shell-*` family of crates. Any domain-specific consumer
(device-family helpers, brand-specific provisioning code, etc.) lives in
a separate repository and depends on these crates via crates.io or git
refs. Such consumers do **not** sit in this workspace.

Current state of this workspace:

- `embedded-shell` — generic, MIT-licensed, intended for crates.io.
  Validated against real hardware (a Linux device and a U-Boot device)
  before this decision was locked in.

Future crates in this workspace: `embedded-shell-transfer` (D-099
Planned) and `embedded-shell-linux` (D-100 Planned).

**Context:** Different concerns deserve different repositories:

1. Different audiences — `embedded-shell-*` is for anyone driving a
   Linux/U-Boot device over serial; downstream consumers are for
   specific device families.
2. Different release cadences — the generic crates can move
   independently.
3. Different licensing posture — generic crates are MIT/Apache and
   crates.io-bound; downstream consumers may want different licensing
   or private hosting.
4. Prevents accidental coupling — physical separation removes the
   temptation to add a domain-specific assumption to a generic crate.
5. Cleaner publishing story — the open repo contains only the generic
   crates intended for crates.io.

**Alternatives considered:**

- Single crate with cargo features (`features = ["brand-x"]`). Rejected:
  features don't compose cleanly across repository boundaries; explicit
  dependency chains are clearer.
- Both in one workspace, separate from the predecessor project.
  Rejected: harder to publish the generic crates while iterating on
  domain-specifics.

**Consequences:**

- Workspace `Cargo.toml` lists only `embedded-shell-*` members.
- Downstream consumers depend on these crates from crates.io once
  published, or via git refs during co-development.
- README.md, CLAUDE.md, and this file describe only the generic family.

---

## D-016 — `LinuxShell` marker trait for shells with a coreutils-like userland

**Decision:** A marker trait `LinuxShell: Shell` (in the `embedded-shell`
crate) tags shells whose device-side userland is Linux-style — that is,
provides `cat`, `ls`, `chmod`, `sh`, `printf`, `base64`, `sha256sum`,
`timeout(1)`, and the rest of the typical coreutils / busybox / toybox
toolkit. Higher-level wrappers built on this tooling — the
`embedded-shell-linux` crate's `fs::*` functions, the
`embedded-shell-transfer` crate's `serial::*` and `http::*` functions —
constrain themselves to `&mut dyn LinuxShell` instead of `&mut dyn Shell`.

Built-in impls: `SubprocessShell`, `LinuxSerialShell`. Notably
**not** impl'd by `UBootSerialShell`, since U-Boot has no `cat`, `sh`,
`base64`, etc.

**Context:** Without this constraint, a user could pass a
`UBootSerialShell` to `fs::read_to_string` or `transfer::serial::push`,
get a `CommandNotFound`-style error at runtime, and have to wonder
whether the operation just isn't supported on their device or whether
they hit a bug. With the marker, the type system rejects the call at
compile time with an unambiguous "the trait bound `UBootSerialShell:
LinuxShell` is not satisfied".

**Alternatives considered:**

- Keep `&mut dyn Shell` everywhere, document the runtime contract.
  Rejected: the type system can catch this mistake cheaply, no reason
  not to.
- Name the trait `PosixShell`. Rejected: the wrappers built on this
  trait use non-POSIX commands (`sha256sum`, `timeout(1)`, `base64`'s
  exact invocation). A strictly-POSIX device (stock macOS, stock
  FreeBSD) wouldn't actually run them. "Linux" is honest about the
  realistic target — embedded Linux distributions and Linux-userland-
  compatible embedded stacks (busybox-based OpenWrt, Buildroot, Yocto;
  toybox-based Android). Embedded macOS / BSD is essentially nonexistent
  in practice.
- Make the trait have actual required methods (e.g.
  `fn has_command(&self, cmd: &str) -> bool`). Rejected: a marker is
  sufficient; the contract is "you promise this shell can run X, Y, Z."

**Consequences:**

- `embedded-shell-linux::fs::*` and `embedded-shell-transfer::*` take
  `&mut dyn LinuxShell`, not `&mut dyn Shell`.
- A future `embedded-shell-uboot` crate would introduce a sibling
  `UBootShell` marker and wrap U-Boot-specific commands (`printenv`,
  `setenv`, `tftpboot`, `loady`/`loadb`). Each device family gets its
  own marker + crate; the patterns are symmetric.
- Custom downstream shells (SSH-backed, container-exec-backed, …) can
  opt in by impl'ing the marker if they satisfy the contract.
- Implemented in `crates/embedded-shell/src/shell/traits.rs`.

---

## D-017 — Framed exec uses idle-after-first-byte read timeout

**Decision:** `LinuxSerialShell::exec_framed` and
`UBootSerialShell::exec_framed` read the framed response via
`SerialTransport::read_until_progressive(predicate, initial, idle)`
rather than a single wall-clock `read_until(predicate, deadline)`.
The `initial` deadline covers the silent-execution phase (the user's
command is running, device hasn't started catting `/tmp/out` yet);
the `idle` deadline covers the streaming phase — once any byte
arrives, the deadline resets on every chunk, so a steady stream of
output keeps the read alive indefinitely. Currently `initial =
command.timeout + 2s` and `idle = 5s` on both shells.

**Context:** The original `read_until(predicate, timeout + 2s)` used
a single wall-clock deadline. For output-heavy commands like
`journalctl` on a busy device, the protocol legitimately needs many
seconds to dump the captured `/tmp/out` over a 115 200-baud line —
even though the device is steadily writing bytes the whole time. The
host would declare the transport dead at 7 s and escalate to a full
re-activate, which then frequently failed because the device was
still mid-dump. The user found this on their first real REPL
session.

**Alternatives considered:**

- Just bump the wall-clock cap (e.g. `timeout + 30s`). Rejected:
  inflates the upper-bound failure latency for short commands and
  doesn't address the symptom — a slow, large `cat` still hits the
  cap if the file is big enough.
- Switch to streaming framing (per-line tagged frames). Rejected:
  larger protocol change with its own caveats (line-buffer
  prerequisite, busybox edge cases). The async-first imperative
  driver doesn't need real-time streaming; it needs not to time
  out while bytes are flowing.
- Use a heartbeat from the device. Rejected: requires device-side
  cooperation; multiple shell families would each need a heartbeat
  convention.

**Consequences:**

- `Shell::run()` no longer times out when the device is steadily
  producing output, even if the *total* time exceeds the wall-clock
  cap of the old `read_until`.
- Worst-case latency when a command genuinely hangs is now
  `initial + idle` (≈ timeout + 7 s) instead of `timeout + 2 s`.
  A small slip; the new behaviour is correct rather than tight.
- `SerialTransport::read_until` (the wall-clock version) stays in
  place — `LinuxSerialShell::activate` and similar prompt-detection
  flows want wall-clock semantics because they shouldn't accept
  "device is still spewing kernel chatter" as a sign of liveness.
- Three new unit tests in `serial.rs` pin the new method's
  behaviour (succeeds under steady stream, times out on initial
  silence, times out on idle after a burst).

---

## D-099 — `embedded-shell-transfer` crate (push/fetch, multi-transport)

**Decision:** A third workspace crate for file push/fetch, implementing
**multiple transports as module-namespaced functions** behind Cargo
features:

- `http::push` / `http::fetch` — fast path: embedded `hyper` server on
  the host, `wget` (preferred) or `curl` on the device. Default feature.
- `serial::push` / `serial::fetch` — slow but works without network:
  base64 encode → ship over the shell line → base64 decode device-side.
  No network deps.
- Future: TFTP, U-Boot `loady`/`loadb` Y/Xmodem.

**Context:** Push/fetch isn't a serial transport — it's an application
on top of the shell. It deserves its own crate because:

1. It has heavy deps (`hyper`, `local-ip-address`) that shell-only users
   shouldn't pay for.
2. The serial-transport variant is genuinely useful for bootstrap
   scenarios where the device has no working network yet (no SSH
   daemon, no DHCP, no DNS — initial provisioning). The host can still
   move small payloads over the serial line.
3. Multiple transports live alongside each other cleanly with feature
   flags.

**Alternatives considered:**

- A `PushFetch` trait with strategy impls. Rejected: transports take
  different arguments (HTTP needs host-IP discovery, serial needs
  nothing extra). Hiding that behind a uniform interface either takes
  irrelevant args or has surprising failure modes.
- A single `push(strategy: TransferStrategy)` enum dispatch. Same problem.
- Putting push/fetch into a downstream domain-specific crate.
  Rejected: it's a generic capability — any consumer with a Linux device
  + serial line wants this. Belongs in the `embedded-shell-*` family.
- Putting push/fetch into `embedded-shell`. Rejected: pulls `hyper` into
  every consumer of the shell crate.

**Consequences:**

- Workspace has a third crate (`crates/embedded-shell-transfer`).
- Downstream consumers depend on it for any provisioning flows that
  move bytes between host and device.
- Both transports are implemented (HTTP + serial). Functions take
  `&mut dyn LinuxShell` (see D-016) — `UBootSerialShell` is excluded at
  compile time.
- Status reporting via `info!`/`debug!` events documented in the
  crate's `lib.rs` module doc; messages and field names are public API.

---

## D-100 — `embedded-shell-linux` crate (thin CLI wrappers)

**Decision:** A fourth workspace crate for thin wrappers around
common Linux userland CLI tools (`ls`, `cat`, `chmod`, `mkdir`, `rm`,
`sha256sum`, `ping`, `systemctl`, `nmcli`, `mmcli`, ...). Grouped behind
Cargo features by **system package** (not per-command):

```toml
[features]
default = ["coreutils", "iputils"]      # universal on any embedded Linux
coreutils      = []
iputils        = []
systemd        = []                      # opt-in: not on minimal distros
networkmanager = []                      # opt-in: heavier parsing
modemmanager   = []                      # opt-in
iproute2       = []                      # opt-in
```

Default features are limited to what's universal on embedded Linux
(busybox or GNU coreutils, ping). Systemd is intentionally opt-in
because many embedded distros (OpenWrt, busybox-init Buildroot variants,
Alpine without OpenRC, etc.) don't ship it.

**Context:** Each wrapper is small (10-30 lines) but the parsing surface
varies. Users who need basic file ops shouldn't compile the NetworkManager
output parser; users who only manage services shouldn't compile coreutils
wrappers.

**Alternatives considered:**

- Per-command features (`ls`, `cat`, `chmod`, …). Rejected: too granular;
  users would have to enable five features for basic file operations.
- One feature per functional area (`fs`, `net`, `system`). Rejected: tools
  cross functional categories (`hostnamectl` is from `systemd` but is
  conceptually network/identity). Per-system-package is more honest.
- Folding all wrappers directly into `embedded-shell`. Rejected: keeps the
  foundation crate slim and focused on the transport abstraction.

**Consequences:**

- Workspace has a fourth crate (`crates/embedded-shell-linux`).
- The crate's feature list doubles as a catalog of what's wrapped.
- Adding a new system package's wrappers (e.g. `iptables`, `nftables`) is
  a feature addition, not a breaking change.
- All wrappers take `&mut dyn LinuxShell` (see D-016), excluding
  `UBootSerialShell` at compile time.
- Implementation status: all wrapper modules (`fs`, `iputils`,
  `systemd`, `journalctl`, `iproute2`, `networkmanager`, `modemmanager`)
  are implemented and exercised by both in-process unit tests and
  opt-in hardware tests. Each crate feature documented in the
  per-crate README under `crates/embedded-shell-linux`.
