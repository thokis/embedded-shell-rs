# `embedded-shell-cmdtree`

Hierarchical command-tree shell engine for `embedded-shell-rs`,
inspired by MikroTik RouterOS. Lets you navigate a tree of typed
commands (`/info`, `/network`, …) with tab-completion and inline help
instead of typing raw shell lines. Part of the
[`embedded-shell-rs`](../..) workspace.

Different persona than [`embedded-shell-cli`](../embedded-shell-cli):
`eshell` is for *scripting* the device (one shot per subcommand);
`etree` is for *navigating* the device (interactive REPL, exploratory).
Same underlying transport.

## What's in the crate

- **`CommandTree`** — the root tree. Build at startup with `add` and
  `mount`, then hand to `Repl::new`.
- **`Node` / `Leaf`** — branches and runnable endpoints. Per-node help
  text powers both `?` introspection and tab-completion hints.
- **`Handler` trait** — what a leaf does. Implementors take a
  `&mut dyn LinuxShell` and call into the transport. Async via
  `async_trait`.
- **`Repl`** — `rustyline`-backed REPL with a tree-aware completer.
- **`demo` module** (feature-gated) — reference tree with `/info`
  and `/network`. The bundled `etree` binary runs it.

## Running the bundled binary

```sh
# Local-host mode (SubprocessShell) — try the demo against your own machine
etree

# Real device
etree -p /dev/ttyUSB0
ETREE_PORT=/dev/ttyUSB0 etree
```

Inside the REPL:

```
etree shell. type `?` to list nodes, `\quit` (or Ctrl-D) to exit.
/> ?
…
/> /info
…
/> /network?       # introspect without running
…
/> \quit
```

`<TAB>` completes node paths.

## Building your own tree (downstream)

```toml
[dependencies]
embedded-shell-cmdtree = { version = "0.1", default-features = false }
embedded-shell = "0.1"
```

```rust
use embedded_shell_cmdtree::{CommandTree, Leaf, Repl, Handler, Invocation};
use embedded_shell::shell::LinuxShell;
use async_trait::async_trait;
use anyhow::Result;

struct MyHandler;
#[async_trait]
impl Handler for MyHandler {
    async fn invoke(&self, _: &Invocation, shell: &mut dyn LinuxShell) -> Result<()> {
        // ... do something with the shell
        Ok(())
    }
}

let mut tree = CommandTree::new();
tree.add("/something/specific", "what this node does", Leaf::new(MyHandler));
// open a shell however you like
Repl::new(tree, my_shell).run().await?;
```

`CommandTree::mount(path, subtree)` grafts a pre-built subtree under
`path` — useful when several teams maintain separate `CommandTree`
modules that get composed at the binary level.

## Stability

Pre-1.0 (0.x). The following are intended to remain stable across
0.x releases; everything else may change:

- `CommandTree::{new, add, mount}` signatures
- `Handler` trait signature
- `Leaf::new`, `Invocation::path`
- `Repl::{new, run, with_history}`

A 1.0 release will commit the full public API. Parameter types, typed
value completion, and per-node sub-state (RouterOS-style "edit one
item at a time") are explicitly **not** in v1 — they're the obvious
next layer once a downstream consumer has concrete needs.

## License

MIT — see [`LICENSE`](../../LICENSE) in the workspace root.
