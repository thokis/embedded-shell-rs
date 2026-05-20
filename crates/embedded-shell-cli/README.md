# `eshell` — command-line driver for `embedded-shell-rs`

A small CLI that talks to embedded Linux devices over a serial line.
Use it like `ssh`/`scp` but for serial-attached devices: run a command,
push/pull a file, get a one-page health summary, ping a target,
reboot.

## Install

```sh
cargo install --path crates/embedded-shell-cli
```

The crate is `embedded-shell-cli`; the installed binary is `eshell`.

## Picking the target

The target is a **global flag**, not a positional argument:

```sh
eshell -p /dev/ttyUSB0 <subcommand> [args…]   # serial device
ESHELL_PORT=/dev/ttyUSB0 eshell <subcommand>  # same thing, via env
eshell <subcommand> [args…]                   # local host (SubprocessShell)
```

Omitting `-p`/`--port` (and not setting `ESHELL_PORT`) runs the
subcommand against the **local host** via `SubprocessShell` — same code
paths, no serial line involved. Useful for trying the tool out without
a device attached, and for running the wrapper code against the host's
own systemd / network stack.

`push`, `pull`, and `reboot` refuse local mode: the blast radius
against the host's filesystem or boot state is too large for an
ergonomic shortcut. You'll get a clear error if you try.

## Subcommands

| Command | What it does |
|---|---|
| `eshell exec -- argv…` | Run one command on the device, mirror stdout/stderr/exit code. `--json` for structured output. |
| `eshell push --src ... --dst ... [--mode 0644] [--via http\|serial]` | Push a local file to the device. Default is HTTP-first with serial fallback. |
| `eshell pull --src ... --dst ... [--via http\|serial]` | Fetch a remote file. Same fallback policy as `push`. |
| `eshell info [--json]` | Pretty-prints OS, kernel, uptime, memory, root-fs usage, IPv4. |
| `eshell ping TARGET [--count N] [--json]` | Ping `TARGET` from the device. Exits 1 on total loss so scripts can branch. |
| `eshell reboot` | Reboot the device, wait for it to come back. Reports how long that took. |
| `eshell service UNIT <action>` | Systemd unit control. `<action>` is one of `status` / `start` / `stop` / `restart` / `reload` / `enable` / `disable`. `status` returns structured info (and exits 3 if not active, mirroring `systemctl is-active`); `--json` for the status flavor. |
| `eshell services [--pattern P] [--failed-only] [--json]` | Tabular listing of systemd units. Default shows every active unit; `--pattern '*.service'` to filter, `--failed-only` to show just the broken ones. |
| `eshell journal [--unit U] [-n N] [--since EXPR] [--json]` | Tail the systemd journal. Filters compose: `--unit foo --since "1 hour ago"` gives that unit's last hour. Default is the last 50 entries from everything. `--json` emits JSONL. |
| `eshell modem [-m INDEX] [--no-sim] [--json]` | Modem + primary-SIM details from ModemManager. Without `-m`, the first modem mmcli reports is used; pass `-m 1` (etc.) on multi-modem devices. `--no-sim` skips the SIM lookup. |
| `eshell network [--json]` | Comprehensive network state: kernel-view (`ip -j` links/addresses/routes) and NM-view (`nmcli` connections) side by side. Gracefully degrades to NM-only on devices whose `ip` lacks JSON support. |

Prepend `-p PORT` (or set `ESHELL_PORT`) to target a serial device.

## Local-mode examples

```sh
eshell info                                  # local host's OS / uptime / IPv4
eshell services --failed-only                # this laptop's failed units
eshell journal --unit sshd.service -n 20     # local journal
eshell ping 8.8.8.8 --count 2                # ping from this host
eshell exec -- sh -c 'free -h | head -2'     # arbitrary command, local
```

## Transport fallback for push/pull

By default `push` and `pull` try HTTP first because it's ~1000× faster.
If HTTP fails for an HTTP-specific reason — no usable host IP, no
`wget` or `curl` on the device, hyper-layer error — the CLI falls back
to the serial transport automatically and logs a `WARN` event saying
so.

Failures rooted in the device or the file (permission denied, source
missing, checksum mismatch) **do not** trigger the fallback — the same
failure would happen on either transport, and masking it would just
take longer.

Force a specific transport with `--via http` or `--via serial`.

**Caveat for serial:** push payloads are capped at 64 KiB (the limit
of a single shell command line). Large payloads with HTTP failing will
fail loudly.

## Passwords

The CLI accepts a login password via:

```sh
eshell --password 'secret' -p /dev/ttyUSB0 info
# or
ESHELL_PASSWORD=secret eshell -p /dev/ttyUSB0 info
```

The env-var path is preferred in scripts so the password doesn't end
up in shell history. `--password` is a global flag — it works on every
subcommand.

## Tracing

`eshell` installs a `tracing-subscriber` filtering on `RUST_LOG`
(default `warn`). To see the library's lifecycle events:

```sh
RUST_LOG=embedded_shell=info,embedded_shell_transfer=info eshell -p /dev/ttyUSB0 push \
    --src ./config.json --dst /etc/app.cfg --mode 0644
```

`embedded_shell=debug` adds operation-level events; `=trace` adds
byte-level RX/TX of the serial line.

## Examples

```sh
# Run something on the device
eshell -p /dev/ttyUSB0 exec -- uname -a
eshell -p /dev/ttyUSB0 exec -- sh -c 'systemctl is-active sshd'

# Push a config and chmod it
eshell -p /dev/ttyUSB0 push --src ./app.cfg --dst /etc/app.cfg --mode 0644

# Pull a log
eshell -p /dev/ttyUSB0 pull --src /var/log/messages --dst .

# Health snapshot, as JSON for piping to jq
eshell -p /dev/ttyUSB0 info --json | jq .

# Smoke-test connectivity, scripted
if eshell -p /dev/ttyUSB0 ping 8.8.8.8 --count 2 >/dev/null; then
    echo "online"
fi

# Reboot and verify it comes back
eshell -p /dev/ttyUSB0 reboot

# Check a service, then restart it
eshell -p /dev/ttyUSB0 service sshd.service status
eshell -p /dev/ttyUSB0 service sshd.service restart

# Last hour of logs from one unit, JSONL for pipeline use
eshell -p /dev/ttyUSB0 journal --unit sshd.service --since "1 hour ago" --json | jq .

# Modem inventory — pipeline-friendly JSON
eshell -p /dev/ttyUSB0 modem --json | jq '{model, signal_quality, operator: .operator_name}'

# Failed services on this device
eshell -p /dev/ttyUSB0 services --failed-only

# Network state in one call
eshell -p /dev/ttyUSB0 network
```

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Generic CLI failure (couldn't open shell, bad arguments, etc.) |
| `1` from `ping` | Total packet loss — the device couldn't reach the target |
| `<device's exit code>` from `exec` | The CLI mirrors the device's exit code (clamped to 0–255) |
