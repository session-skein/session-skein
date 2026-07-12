# Session Skein

Session Skein is a fast, local-first control plane for Codex CLI projects, threads,
and activity. It works directly with the locally installed Codex CLI and its existing
authentication. Planned optional adapters may enrich the view when tools such as tmux
or Agent Deck are installed, but neither will be required.

The project is in an early alpha phase. Today, the `skein` binary provides a
secure versioned project registry, diagnostics, bounded incremental Git metadata, and
a durable Codex session catalog populated through the app-server protocol. It can
run one audited foreground Codex turn against an explicitly selected project or
thread. The alpha worker path can keep an explicitly targeted turn alive while CLI
clients disconnect and reconnect. It can rank registered projects and linked sessions
from one private stdin query, dispatch a unique high-confidence route through a
reconnectable Codex worker, and render factual project/day activity views. Its
standalone TUI puts those views and the global conductor composer in one terminal.

## Why a skein?

Individual agent sessions are threads. Projects, decisions, worktrees, and history
weave those threads into something larger. Session Skein keeps that structure local,
inspectable, and recoverable without treating one giant prompt as permanent memory.

## Current commands

```console
cargo run --release --bin skein -- doctor
cargo run --release --bin skein -- init
cargo run --release --bin skein -- project add /path/to/project
cargo run --release --bin skein -- scan-root add /path/to/workspace --recursive
cargo run --release --bin skein -- index
cargo run --release --bin skein -- project refresh /path/to/project
cargo run --release --bin skein -- project show /path/to/project
cargo run --release --bin skein -- project list
cargo run --release --bin skein -- import codex preview --limit 50
cargo run --release --bin skein -- session sync codex --all-pages --limit 100
cargo run --release --bin skein -- session list
cargo run --release --bin skein -- session show THREAD_ID
cargo run --release --bin skein -- session bind THREAD_ID /path/to/project
cargo run --release --bin skein -- session unbind THREAD_ID
printf '%s\n' 'Describe this repository.' | \
  cargo run --release --bin skein -- control codex /path/to/project \
    --full-access --jsonl
cargo run --release --bin skein -- control list
cargo run --release --bin skein -- control show RUN_ID
cargo run --release --bin skein -- control mark-stale --force
printf '%s\n' 'Run the focused tests.' | \
  cargo run --release --bin skein -- worker start /path/to/project \
    --full-access
cargo run --release --bin skein -- worker list --active
cargo run --release --bin skein -- worker status RUN_ID
cargo run --release --bin skein -- worker watch RUN_ID --jsonl
printf '%s\n' 'Change direction without starting a new turn.' | \
  cargo run --release --bin skein -- worker steer RUN_ID
cargo run --release --bin skein -- worker interrupt RUN_ID
cargo run --release --bin skein -- worker read RUN_ID
cargo run --release --bin skein -- worker reconcile RUN_ID
cargo run --release --bin skein -- worker stop RUN_ID
printf '%s\n' 'continue Session Skein routing work' | \
  cargo run --release --bin skein -- conduct --full-access
printf '%s\n' 'continue the renderer investigation' | \
  cargo run --release --bin skein -- match
cargo run --release --bin skein -- summary project /path/to/project
cargo run --release --bin skein -- summary projects
cargo run --release --bin skein -- summary day
cargo run --release --bin skein -- tui
cargo run --release --bin skein -- mcp

# Add --format json anywhere when a script needs structured output:
cargo run --release --bin skein -- project list --format json
```

`doctor` is always read-only and does not migrate an older database. `init` creates
or upgrades the private per-user SQLite database. Project discovery is explicit.
Register one exact project with `project add`, or approve a discovery root:

```console
skein scan-root add /path/to/workspace
skein scan-root add /path/to/workspace --recursive --max-depth 16
skein scan-root list
skein index
skein search project terms
skein context status
skein context memories enable
skein context sessions enable
skein context refresh
```

Recursion is opt-in. It never follows directory symlinks, recognizes normal Git
repositories and worktree `.git` files, and prunes common dependency, build, cache,
vendor, and virtual-environment directories. A discovered Git repository is a
recursion boundary, so source trees are not needlessly crawled. The default recursive depth is 16 and
the maximum is 64. An unavailable network root is reported without removing its
cached projects, and the stored root can still be removed while unmounted.

Human-readable hierarchical output is the default. Use the global
`--format json` option for automation; the older per-command `--json` switch remains
available for compatibility. Streaming commands continue to use `--jsonl`.

`index` also builds a private, bounded project-identity FTS index from Git-tracked
README variants, `AGENTS.md`, common manifests, and top-level Markdown in `docs/`
and `.codex/`. If Git file enumeration fails, it uses only this fixed bounded path
set, which may include untracked files. It reads at most 40 files, 64 KiB per file,
and 512 KiB per project; it never follows symlinks. `search` and the MCP recall tools return only bounded
snippets and source paths, never complete indexed documents.

Deep recall is disabled by default. Generated memory summaries can be enabled
separately. Raw session recall additionally requires the session cwd to be beneath a
persisted approved scan root; only user/assistant message text is admitted. Source
files, imported documents, returned snippets, and total refresh work are all bounded.
Disabling a source and refreshing atomically removes its private context documents.
See [docs/context-recall.md](docs/context-recall.md).

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
inspect durable redacted state, reattach to a bounded memory-only event window,
steer or interrupt the exact active turn without handling thread or turn IDs, and
read redacted source status. A fenced lost worker can be reconciled against the exact
recorded Codex turn; terminal source truth closes the run, while an in-progress or
missing turn remains recovery-required. No work is replayed or taken over. See
[docs/workers.md](docs/workers.md).

`match` is the read-only decision layer beneath the conductor. It reads a
bounded query from stdin, ranks only explicitly registered projects and linked
sessions, and reports every scoring contribution. Recency can strengthen a lexical or
exact-identity match but cannot nominate a project by itself. Recommendations are
non-dispatching in `match`; `conduct` independently re-evaluates the same route inside
its audited planning transaction. `summary project` and `summary day` assemble deterministic
factual prose from metadata already in SQLite; they do not launch Codex, Git, an LLM,
or scan a repository. See [docs/matching-summaries.md](docs/matching-summaries.md).

`conduct` reads one prompt once, refuses anything weaker than a unique high-confidence
route, verifies ChatGPT authentication, then atomically records content-free evidence,
full-access policy, control actions, and a starting worker claim before process spawn.
`--request-id UUID` makes retries status-only: Skein never resends lost private prompt
content. See [docs/conductor.md](docs/conductor.md).

`tui` is the standalone keyboard-first interface. It reads the same private local
registry, invokes `skein conduct` as a bounded child process, and follows redacted
worker events. A dispatch requires pressing F2 immediately before Enter; that
authority is consumed once. Agent Deck, tmux, MCP, systemd, and API keys are not
required. See [docs/tui.md](docs/tui.md).

`mcp` is the on-demand stdio adapter for Codex CLI. It exposes the same private local
registry and audited control paths without a daemon or another credential. Register
the installed binary once, then start a new Codex session:

```console
codex mcp add session-skein -- skein mcp --allow-control
codex mcp get session-skein --json
```

The project-recall names used by the former Codex Brain server remain available,
including `search_projects`, `get_project`, `suggest_codex_command`, `refresh_index`,
`refresh_activity`, and the scan-root tools. `add_scan_root` accepts an explicit
`recursive` policy and `refresh_index` discovers repositories before refreshing Git
metadata. Defaults-off, bounded generated-memory and raw user/assistant session recall
are available explicitly. Native tools add session/run inspection and audited
conduct, steer, interrupt, and reconciliation operations. See [docs/mcp.md](docs/mcp.md).

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
