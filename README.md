# Session Skein

Session Skein is a fast, local-first control plane for Codex CLI projects, threads,
and activity. It works directly with the locally installed Codex CLI and its existing
authentication. Planned optional adapters may enrich the view when tools such as tmux
or Agent Deck are installed, but neither will be required.

The project is in an early foundation phase. Today, the `skein` binary provides a
secure versioned project registry, diagnostics, bounded incremental Git metadata, and
a durable Codex session catalog populated through the app-server protocol. It can
run one audited foreground Codex turn against an explicitly selected project or
thread. The alpha worker path can keep an explicitly targeted turn alive while CLI
clients disconnect and reconnect. It does not yet route natural-language work or
provide the conductor TUI.

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
printf '%s\n' 'Describe this repository.' | \
  cargo run --release --bin skein -- control codex /path/to/project \
    --full-access --jsonl
cargo run --release --bin skein -- control list --json
cargo run --release --bin skein -- control show RUN_ID --json
cargo run --release --bin skein -- control mark-stale --force --json
printf '%s\n' 'Run the focused tests.' | \
  cargo run --release --bin skein -- worker start /path/to/project \
    --full-access --json
cargo run --release --bin skein -- worker list --active --json
cargo run --release --bin skein -- worker status RUN_ID --json
cargo run --release --bin skein -- worker watch RUN_ID --jsonl
cargo run --release --bin skein -- worker interrupt RUN_ID
cargo run --release --bin skein -- worker stop RUN_ID
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

`control codex` reads its prompt from standard input, records an immutable
danger-full-access / never-approve policy snapshot before mutation, verifies that
Codex applied that policy, then streams one turn until authoritative completion. Live
content is redacted unless `--include-content` is supplied and is never stored by
Session Skein. Resume a selected thread with `--resume THREAD_ID`. See
[docs/codex-control.md](docs/codex-control.md).

`worker start` and `worker resume` create one on-demand Skein worker per run. The
worker owns the Codex stdio connection, so the starting CLI or a watcher can exit
without stopping the turn. Fresh CLI processes discover jobs with `worker list`,
inspect durable redacted state, reattach to a bounded memory-only event window, and
interrupt the exact active turn without handling thread or turn IDs. See
[docs/workers.md](docs/workers.md).

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
tmux, an MCP client, systemd, a separately installed service, or a separate API key.
Reconnectable jobs use an on-demand background Skein process that exits after the job
is terminal and idle. Planned optional integrations will be capability-detected at
runtime and will never own Session Skein state.

## License

MIT. See [LICENSE](LICENSE).
