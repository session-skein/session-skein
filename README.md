# Session Skein

Session Skein is a fast, local-first control plane for Codex CLI projects, threads,
and activity. It works directly with the locally installed Codex CLI and its existing
authentication. Planned optional adapters may enrich the view when tools such as tmux
or Agent Deck are installed, but neither will be required.

The project is in an early foundation phase. Today, the `skein` binary provides a
secure versioned project registry, diagnostics, bounded incremental Git metadata, and
a durable Codex session catalog populated through the app-server protocol. It does
not yet control Codex workers or provide the conductor TUI.

## Why a skein?

Individual agent sessions are threads. Projects, decisions, worktrees, and history
weave those threads into something larger. Session Skein keeps that structure local,
inspectable, and recoverable without treating one giant prompt as permanent memory.

## Current commands

```console
cargo run --release --bin skein -- doctor
cargo run --release --bin skein -- init
cargo run --release --bin skein -- project add /path/to/project
cargo run --release --bin skein -- project refresh /path/to/project --json
cargo run --release --bin skein -- project refresh /path/to/project --working-tree --json
cargo run --release --bin skein -- project show /path/to/project --json
cargo run --release --bin skein -- project list --json
cargo run --release --bin skein -- import codex preview --limit 50 --json
cargo run --release --bin skein -- session sync codex --limit 50 --json
cargo run --release --bin skein -- session list --json
cargo run --release --bin skein -- session show THREAD_ID --json
cargo run --release --bin skein -- session bind THREAD_ID /path/to/project --json
cargo run --release --bin skein -- session unbind THREAD_ID --json
```

`doctor` is always read-only and does not migrate an older database. `init` creates
or upgrades the private per-user SQLite database. Project discovery is explicit;
Session Skein does not recursively crawl disks or network mounts.

The default `project refresh` reads small Git administrative files and the latest
commit, then stores a fingerprint. A second refresh skips Git entirely when that
fingerprint is unchanged. Working files are not scanned unless `--working-tree` is
specified; that opt-in check covers tracked files and deliberately excludes untracked
files and submodules. Use `--all` explicitly to refresh every registered project and
`--force` to bypass the fingerprint. See [docs/git-refresh.md](docs/git-refresh.md).

Codex preview uses the locally installed `codex app-server` and its existing account
authentication. It reads one bounded thread-list page and never writes Session Skein
state. Names and first-message previews are omitted unless `--include-text` is given.
The default uses Codex's state database only; `--repair-source-index` explicitly opts
into Codex's slower JSONL scan-and-repair path. See
[docs/codex-preview.md](docs/codex-preview.md).

`session sync codex` makes the explicit transition from a write-free preview to a
bounded, transactional metadata synchronization. It stores no thread text by default,
never parses rollout JSONL, and never silently creates projects. Existing registered
projects are associated using the longest canonical ancestor of the observed Codex
cwd; unmatched sessions remain visible. See [docs/session-sync.md](docs/session-sync.md).

Environment overrides:

- `SKEIN_CONFIG_DIR` changes the configuration directory.
- `SKEIN_DATA_DIR` changes the private state directory.

## Development

Rust 1.95 or newer is required. The repository pins 1.95.0 for reproducible checks.

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release
```

See [ROADMAP.md](ROADMAP.md), [docs/architecture.md](docs/architecture.md), and
[docs/privacy.md](docs/privacy.md) before proposing a new integration.

## Independence

Session Skein is an independent open-source project. It is not affiliated with or
endorsed by OpenAI. Codex and OpenAI are trademarks of their respective owner.

Codex CLI is the first-class agent runtime. Session Skein does not require Agent Deck,
tmux, an MCP client, a background service, or a separate API key. Planned optional
integrations will be capability-detected at runtime and will never own Session Skein
state.

## License

MIT. See [LICENSE](LICENSE).
