//! Hierarchical command-tree shell engine.
//!
//! Inspired by MikroTik RouterOS — the user navigates a tree of typed
//! commands (`/info`, `/network`, …) rather than typing raw shell
//! lines. Tab-completion walks the tree; `?` introspects.
//!
//! # Layers
//!
//! - [`CommandTree`] — the tree of [`Node`]s.
//! - [`Leaf`] — a runnable node, wrapping a [`Handler`].
//! - [`Repl`] — `rustyline`-backed REPL that drives the tree against an
//!   [`embedded_shell::shell::LinuxShell`].
//!
//! Downstream crates build their own [`CommandTree`] and pass it to
//! [`Repl::new`]. The reference impl in this crate (under the `demo`
//! feature) mounts `/info` and `/network` and is what the bundled
//! `etree` binary runs.

#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use embedded_shell::shell::LinuxShell;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};

#[cfg(feature = "demo")]
pub mod demo;

// ---------------------------------------------------------------------
// Handler trait
// ---------------------------------------------------------------------

/// Behaviour attached to a [`Leaf`].
///
/// Implementors are typically zero-sized structs (`InfoHandler`,
/// `NetworkHandler`, …) but can carry state if a node needs it. The
/// invocation gets a `&mut dyn LinuxShell` so it can run commands /
/// read files / etc. through the shared transport, plus a writer to
/// emit human-readable output (the REPL prints it to stdout; an
/// HTTP server captures it for the response body; a runbook engine
/// could capture it for a report).
///
/// The writer is `Send` so handlers stay compatible with
/// multi-threaded async runtimes (e.g. axum). For per-command
/// buffering — call site allocates a `Vec<u8>`, hands it in, then
/// dumps the result wherever it's going.
#[async_trait]
pub trait Handler: Send + Sync {
    /// Run the leaf. Errors bubble up to the caller, which decides
    /// how to surface them (the REPL renders on stderr and continues
    /// to the next prompt).
    async fn invoke(
        &self,
        inv: &Invocation,
        shell: &mut dyn LinuxShell,
        out: &mut (dyn std::io::Write + Send),
    ) -> Result<()>;
}

// ---------------------------------------------------------------------
// Invocation
// ---------------------------------------------------------------------

/// Parsed user input ready to dispatch.
///
/// Carries the path the user invoked (e.g. `info`, `services/print`)
/// and any trailing whitespace-separated tokens after the path
/// (`set LogLevel=Debug` → path `set`, args `["LogLevel=Debug"]`).
/// Handlers that take parameters parse `args` themselves — the engine
/// stays parameter-shape-agnostic in v1.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// The full normalized path (e.g. `info`, `services/print`).
    pub path: String,
    /// Whitespace-separated tokens after the path. Empty when the
    /// invocation has no trailing args.
    pub args: Vec<String>,
}

// ---------------------------------------------------------------------
// Tree / Node / Leaf
// ---------------------------------------------------------------------

/// One node in the [`CommandTree`].
///
/// A node is either a branch (children only) or a leaf (a [`Handler`])
/// — never both in v0. Help text is per-node and used by the `?`
/// introspection and by tab-completion hover hints.
pub struct Node {
    help: String,
    children: BTreeMap<String, Node>,
    leaf: Option<Leaf>,
}

/// A runnable endpoint in the tree.
pub struct Leaf {
    handler: Box<dyn Handler>,
}

impl Leaf {
    /// Wrap a [`Handler`] in a [`Leaf`].
    pub fn new(handler: impl Handler + 'static) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }
}

impl Node {
    fn branch() -> Self {
        Self {
            help: String::new(),
            children: BTreeMap::new(),
            leaf: None,
        }
    }

    /// Human-readable help text for this node (empty for pure branches
    /// that nobody bothered to document).
    pub fn help(&self) -> &str {
        &self.help
    }

    /// Iterate `(name, child)` pairs in lexicographic order. Used by
    /// non-REPL frontends (web UI, runbook engine) that need to render
    /// children without poking at private fields.
    pub fn children(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.children.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// `true` if this node has a runnable [`Leaf`]. Branches return
    /// `false`; dual-mode nodes (leaf + children) return `true`.
    pub fn is_leaf(&self) -> bool {
        self.leaf.is_some()
    }

    /// Run the node's handler, if any. Returns `Ok(false)` for a pure
    /// branch (nothing to run); `Ok(true)` after a successful invoke;
    /// `Err` if the handler itself errored.
    pub async fn invoke(
        &self,
        inv: &Invocation,
        shell: &mut dyn LinuxShell,
        out: &mut (dyn std::io::Write + Send),
    ) -> Result<bool> {
        match &self.leaf {
            Some(leaf) => {
                leaf.handler.invoke(inv, shell, out).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// The shell's command tree. Build it once at startup with [`add`] and
/// [`mount`], then hand it to [`Repl::new`].
///
/// [`add`]: CommandTree::add
/// [`mount`]: CommandTree::mount
pub struct CommandTree {
    root: Node,
}

impl CommandTree {
    /// Create an empty tree with only the root node.
    pub fn new() -> Self {
        Self {
            root: Node::branch(),
        }
    }

    /// Add a leaf at `path` (e.g. `/info`, `/services/print`).
    /// Intermediate branch nodes are auto-created. Replaces an
    /// existing leaf at the same path.
    pub fn add(&mut self, path: &str, help: impl Into<String>, leaf: Leaf) -> &mut Self {
        let segments = path_segments(path);
        let mut cursor = &mut self.root;
        for (i, seg) in segments.iter().enumerate() {
            let last = i + 1 == segments.len();
            let child = cursor
                .children
                .entry((*seg).to_string())
                .or_insert_with(Node::branch);
            if last {
                child.help = help.into();
                child.leaf = Some(leaf);
                return self;
            }
            cursor = child;
        }
        // Empty path is invalid for add(); ignore silently.
        self
    }

    /// Mount a sub-tree under `path`. Used by downstream crates to
    /// graft their own command vocabulary onto the engine's empty
    /// (or demo-populated) root.
    pub fn mount(&mut self, path: &str, sub: CommandTree) -> &mut Self {
        let segments = path_segments(path);
        let mut cursor = &mut self.root;
        for seg in &segments {
            cursor = cursor
                .children
                .entry((*seg).to_string())
                .or_insert_with(Node::branch);
        }
        // Merge `sub.root.children` into `cursor.children`.
        for (name, node) in sub.root.children {
            cursor.children.insert(name, node);
        }
        self
    }

    /// Walk the tree along `path` and return the node, or `None` if
    /// no such path exists. Public so non-REPL frontends (web UI,
    /// runbook engine) can navigate the tree without rebuilding their
    /// own resolver.
    pub fn resolve<'a>(&'a self, path: &[&str]) -> Option<&'a Node> {
        let mut cursor = &self.root;
        for seg in path {
            cursor = cursor.children.get(*seg)?;
        }
        Some(cursor)
    }

    /// Borrow the root [`Node`]. Useful for frontends that want to
    /// render the top of the tree (`GET /` in the web UI, `?` at the
    /// REPL prompt).
    pub fn root(&self) -> &Node {
        &self.root
    }
}

impl Default for CommandTree {
    fn default() -> Self {
        Self::new()
    }
}

fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

// ---------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------

/// Parsed user input. Either an exit request, an introspection
/// (`?`), or a path to invoke (with optional trailing args).
///
/// The shell has one reserved bare word (`quit`) and one reserved
/// operator (`?`); everything else is a path. The path is the input
/// up to the first whitespace; anything after that is split on
/// whitespace into invocation args. Paths use `/` as a separator
/// (leading slash optional — `info` and `/info` are equivalent).
///
/// One way to do each thing (cf. `DESIGN.md` D-005): no `exit`
/// alias for `quit`, no `help` alias for `?`.
#[derive(Debug)]
enum ParsedInput<'a> {
    Empty,
    Exit,
    Inspect(Vec<&'a str>),
    Invoke {
        segments: Vec<&'a str>,
        args: Vec<&'a str>,
    },
}

fn parse_input(line: &str) -> ParsedInput<'_> {
    let line = line.trim();
    if line.is_empty() {
        return ParsedInput::Empty;
    }
    if line == "quit" {
        return ParsedInput::Exit;
    }
    if line == "?" {
        return ParsedInput::Inspect(Vec::new());
    }
    if let Some(prefix) = line.strip_suffix('?') {
        let prefix = prefix.trim();
        return ParsedInput::Inspect(path_segments(prefix));
    }
    // Invocation: split on first whitespace into path + trailing args.
    let (path_part, args_part) = match line.find(char::is_whitespace) {
        Some(idx) => (&line[..idx], line[idx..].trim()),
        None => (line, ""),
    };
    let args = if args_part.is_empty() {
        Vec::new()
    } else {
        args_part.split_whitespace().collect()
    };
    ParsedInput::Invoke {
        segments: path_segments(path_part),
        args,
    }
}

// ---------------------------------------------------------------------
// Rustyline helper: completion + (dummy) highlight/hint/validate
// ---------------------------------------------------------------------

struct TreeHelper {
    // Snapshot of paths used purely for completion candidates. We keep
    // it separate from the live tree so the rustyline helper trait
    // (which requires `Send + Sync`) doesn't have to drag `dyn
    // Handler` along.
    paths: Vec<String>,
}

impl TreeHelper {
    fn from_tree(tree: &CommandTree) -> Self {
        let mut paths = Vec::new();
        collect_paths(&tree.root, "", &mut paths);
        Self { paths }
    }
}

fn collect_paths(node: &Node, prefix: &str, out: &mut Vec<String>) {
    for (name, child) in &node.children {
        let here = format!("{prefix}/{name}");
        if child.leaf.is_some() {
            out.push(here.clone());
        }
        if !child.children.is_empty() {
            collect_paths(child, &here, out);
        }
    }
}

impl Completer for TreeHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // Complete only when the cursor is at end-of-line for v0.
        // Multi-word completion (params) lands when we add params.
        let prefix = &line[..pos];
        let candidates: Vec<Pair> = self
            .paths
            .iter()
            .filter(|p| p.starts_with(prefix) || ensure_leading_slash(prefix, p))
            .map(|p| Pair {
                display: p.clone(),
                replacement: p.clone(),
            })
            .collect();
        Ok((0, candidates))
    }
}

fn ensure_leading_slash(prefix: &str, path: &str) -> bool {
    // Users may type `info` instead of `/info` — match those too.
    !prefix.starts_with('/') && path.trim_start_matches('/').starts_with(prefix)
}

impl Hinter for TreeHelper {
    type Hint = String;
}
impl Highlighter for TreeHelper {}
impl Validator for TreeHelper {}
impl Helper for TreeHelper {}

// ---------------------------------------------------------------------
// REPL
// ---------------------------------------------------------------------

/// Interactive REPL over a [`CommandTree`].
pub struct Repl {
    tree: CommandTree,
    shell: Box<dyn LinuxShell>,
    history_path: Option<PathBuf>,
    /// Greeting shown once when the REPL starts. Set via
    /// [`with_banner`][Self::with_banner].
    banner: Option<String>,
    /// Input prompt rendered on every line. Defaults to `"> "`.
    /// Set via [`with_prompt`][Self::with_prompt].
    prompt: String,
}

impl Repl {
    /// Build a REPL that dispatches `tree` against `shell`.
    pub fn new(tree: CommandTree, shell: Box<dyn LinuxShell>) -> Self {
        Self {
            tree,
            shell,
            history_path: default_history_path(),
            banner: None,
            prompt: "> ".to_string(),
        }
    }

    /// Override the on-disk history location. `None` disables
    /// persistent history entirely.
    pub fn with_history(mut self, path: Option<PathBuf>) -> Self {
        self.history_path = path;
        self
    }

    /// Set the one-shot greeting printed before the first prompt.
    /// Downstream consumers use this to identify their binary.
    pub fn with_banner(mut self, banner: impl Into<String>) -> Self {
        self.banner = Some(banner.into());
        self
    }

    /// Set the input prompt. Default is `"> "`.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    /// Drive the REPL until the user exits (Ctrl-D, `quit`, or `exit`).
    pub async fn run(mut self) -> Result<()> {
        let helper = TreeHelper::from_tree(&self.tree);
        let mut rl: Editor<TreeHelper, rustyline::history::FileHistory> = Editor::new()?;
        rl.set_helper(Some(helper));
        if let Some(p) = &self.history_path {
            let _ = rl.load_history(p);
        }

        let use_color = std::io::stdout().is_terminal() && std::io::stderr().is_terminal();

        if let Some(b) = &self.banner {
            println!("{b}");
        }

        loop {
            let line = match rl.readline(&self.prompt) {
                Ok(l) => l,
                Err(ReadlineError::Interrupted) => continue,
                Err(ReadlineError::Eof) => break,
                Err(e) => {
                    eprintln!("readline: {e}");
                    break;
                }
            };
            let _ = rl.add_history_entry(line.as_str());

            match parse_input(&line) {
                ParsedInput::Empty => continue,
                ParsedInput::Exit => break,
                ParsedInput::Inspect(segments) => match self.tree.resolve(&segments) {
                    Some(node) => print_node_help(&segments, node, use_color),
                    None => eprintln!("no such node: {}", display_path(&segments)),
                },
                ParsedInput::Invoke { segments, args } => {
                    let path_display = display_path(&segments);
                    match self.tree.resolve(&segments) {
                        None => eprintln!("no such node: {path_display}"),
                        Some(node) => match &node.leaf {
                            Some(leaf) => {
                                let inv = Invocation {
                                    path: path_display.clone(),
                                    args: args.iter().map(|s| s.to_string()).collect(),
                                };
                                // Buffer the handler's output, then
                                // flush to stdout in one shot. Keeps
                                // the Handler future `Send` (StdoutLock
                                // is not Send) and matches what an
                                // HTTP server would do.
                                let mut buf: Vec<u8> = Vec::new();
                                let result =
                                    leaf.handler.invoke(&inv, &mut *self.shell, &mut buf).await;
                                if !buf.is_empty() {
                                    let _ = std::io::Write::write_all(&mut std::io::stdout(), &buf);
                                }
                                if let Err(e) = result {
                                    eprintln!("{path_display}: {e:#}");
                                }
                            }
                            None => {
                                eprintln!(
                                    "{path_display} is a branch — type `{path_display}?` to list children"
                                );
                            }
                        },
                    }
                }
            }
        }

        if let Some(p) = &self.history_path {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = rl.save_history(p);
        }
        let _ = self.shell.deactivate().await;
        Ok(())
    }
}

/// Render a path for display: `info` for a single segment,
/// `services/print` for nested. No leading slash — slash is purely
/// a separator. Empty segment list renders as `(root)`.
fn display_path(segments: &[&str]) -> String {
    if segments.is_empty() {
        "(root)".to_string()
    } else {
        segments.join("/")
    }
}

fn print_node_help(segments: &[&str], node: &Node, use_color: bool) {
    let path = display_path(segments);
    println!("{}", bold(&path, use_color));
    if !node.help.is_empty() {
        println!("  {}", node.help);
    }
    if !node.children.is_empty() {
        println!();
        println!("{}", bold("Children", use_color));
        // Pad child names so help text aligns. The `/` suffix means
        // **pure branch** (no leaf — must be navigated into). Dual-mode
        // nodes (leaf + children) render bare so the help text, which
        // documents the leaf's usage, doesn't conflict with the `/`
        // signal. Users discover sub-paths via `name?`.
        let display_names: Vec<(String, &Node)> = node
            .children
            .iter()
            .map(|(name, child)| {
                let pure_branch = child.leaf.is_none() && !child.children.is_empty();
                let display = if pure_branch {
                    format!("{name}/")
                } else {
                    name.clone()
                };
                (display, child)
            })
            .collect();
        let name_w = display_names
            .iter()
            .map(|(d, _)| d.len())
            .max()
            .unwrap_or(0);
        for (display, child) in &display_names {
            println!("  {display:<name_w$}  {help}", help = child.help);
        }
    }
}

fn bold(s: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn default_history_path() -> Option<PathBuf> {
    let state_dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(state_dir.join("etree").join("history"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyHandler;

    #[async_trait]
    impl Handler for DummyHandler {
        async fn invoke(
            &self,
            _: &Invocation,
            _: &mut dyn LinuxShell,
            _: &mut (dyn std::io::Write + Send),
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn path_segments_normalises_slashes() {
        assert_eq!(path_segments("/info"), vec!["info"]);
        assert_eq!(path_segments("//info//"), vec!["info"]);
        assert_eq!(path_segments("info"), vec!["info"]);
        assert!(path_segments("/").is_empty());
        assert_eq!(path_segments("/a/b/c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn tree_add_resolves() {
        let mut tree = CommandTree::new();
        tree.add("/info", "device summary", Leaf::new(DummyHandler));
        assert!(tree.resolve(&["info"]).is_some());
        assert!(tree.resolve(&["info"]).unwrap().leaf.is_some());
        assert!(tree.resolve(&["nope"]).is_none());
    }

    #[test]
    fn tree_add_creates_intermediate_branches() {
        let mut tree = CommandTree::new();
        tree.add("/services/print", "list", Leaf::new(DummyHandler));
        // intermediate branch exists, has no leaf, has the child
        let svc = tree.resolve(&["services"]).unwrap();
        assert!(svc.leaf.is_none());
        assert!(svc.children.contains_key("print"));
        // leaf is reachable
        assert!(tree.resolve(&["services", "print"]).unwrap().leaf.is_some());
    }

    #[test]
    fn mount_grafts_subtree() {
        let mut sub = CommandTree::new();
        sub.add("/print", "list units", Leaf::new(DummyHandler));
        let mut tree = CommandTree::new();
        tree.mount("/services", sub);
        assert!(tree.resolve(&["services", "print"]).is_some());
    }

    #[test]
    fn parse_input_distinguishes_kinds() {
        assert!(matches!(parse_input(""), ParsedInput::Empty));
        assert!(matches!(parse_input("  "), ParsedInput::Empty));
        assert!(matches!(parse_input("quit"), ParsedInput::Exit));
        // `exit` is not a reserved word — it parses as a node lookup.
        // (D-005: one path per operation.)
        assert!(matches!(parse_input("exit"), ParsedInput::Invoke { .. }));
        assert!(matches!(parse_input("?"), ParsedInput::Inspect(_)));
        // Leading slash is optional — both equivalent.
        for input in ["info?", "/info?"] {
            match parse_input(input) {
                ParsedInput::Inspect(s) => assert_eq!(s, vec!["info"]),
                _ => panic!("expected Inspect for `{input}`"),
            }
        }
        for input in ["info", "/info"] {
            match parse_input(input) {
                ParsedInput::Invoke { segments, args } => {
                    assert_eq!(segments, vec!["info"]);
                    assert!(args.is_empty());
                }
                _ => panic!("expected Invoke for `{input}`"),
            }
        }
    }

    #[test]
    fn parse_input_carries_trailing_args() {
        match parse_input("set LogLevel=Debug") {
            ParsedInput::Invoke { segments, args } => {
                assert_eq!(segments, vec!["set"]);
                assert_eq!(args, vec!["LogLevel=Debug"]);
            }
            other => panic!("expected Invoke with args, got {other:?}"),
        }
        // Multi-token args + a nested path.
        match parse_input("config/set foo=1 bar=2") {
            ParsedInput::Invoke { segments, args } => {
                assert_eq!(segments, vec!["config", "set"]);
                assert_eq!(args, vec!["foo=1", "bar=2"]);
            }
            other => panic!("expected Invoke with multi-args, got {other:?}"),
        }
    }

    #[test]
    fn display_path_strips_leading_slash() {
        assert_eq!(display_path(&["info"]), "info");
        assert_eq!(display_path(&["services", "print"]), "services/print");
        assert_eq!(display_path(&[]), "(root)");
    }

    #[test]
    fn collect_paths_walks_leaves_only() {
        let mut tree = CommandTree::new();
        tree.add("/info", "", Leaf::new(DummyHandler));
        tree.add("/services/print", "", Leaf::new(DummyHandler));
        let mut paths = Vec::new();
        collect_paths(&tree.root, "", &mut paths);
        paths.sort();
        assert_eq!(paths, vec!["/info", "/services/print"]);
    }
}
