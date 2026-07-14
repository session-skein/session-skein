# Session Skein

Session Skein is a fast, local-first control plane for Codex CLI projects,
threads, recall, and audited workers. It gives you one project library, one
search surface, one conductor prompt, and one keyboard-first TUI without
requiring tmux, Agent Deck, systemd, an API key, or a second agent runtime.

```text
one prompt                         existing local work
    |                                     |
    v                                     v
conductor -> project + session catalog <- index + Codex app-server
    |                    |
    v                    v
audited Codex worker     CLI / TUI / MCP
```

Session Skein uses the locally installed Codex CLI and its existing ChatGPT
authentication. Project and session metadata stays in a private per-user SQLite
database. Repository contents, generated Codex memories, and raw session messages
are indexed only through explicit, bounded policies.

## Install

Ask Codex:

> Install and configure https://github.com/session-skein/session-skein with the
> control-enabled MCP profile.

Install the current verified preview directly on Linux/macOS:

```console
curl -fsSL https://raw.githubusercontent.com/session-skein/session-skein/main/install.sh |
  bash -s -- --control
```

On Windows PowerShell:

```powershell
$installer = Join-Path $env:TEMP 'session-skein-install.ps1'
Invoke-WebRequest https://raw.githubusercontent.com/session-skein/session-skein/main/install.ps1 -OutFile $installer
& $installer -Control
```

The safe default is catalog-only MCP. `--control` / `-Control` additionally
exposes the audited conduct, steer, interrupt, and reconcile tools. See
[INSTALL.md](INSTALL.md) for one-line installation, source-build behavior,
collision handling, verification, updates, plugin installation, and uninstall.
The normal path downloads the native release archive, validates its published
manifest and SHA-256 checksum, and installs the bundled skill without Git or Rust.
After installing alpha.9 or newer, `skein update --check` checks the approved preview
channel and `skein update` applies a receipt-owned update.

## Five-minute start

Register one exact repository:

```console
skein init
skein project add /path/to/repository
skein index
skein search project words
skein tui
```

Or approve a workspace and discover Git repositories recursively:

```console
skein scan-root add /path/to/workspace --recursive --max-depth 16
skein index
skein summary projects
```

Import existing Codex thread metadata, then use one prompt as the conductor:

```console
skein session sync codex --all-pages
printf '%s\n' 'continue the renderer investigation' | \
  skein conduct --full-access --follow
```

After explicitly enabling private Codex recall, find and resume an exact prior thread
from natural-language terms. Generated rollout summaries can identify sessions even
when their raw transcript cwd is outside approved repository roots:

```console
skein context sessions enable
# Or enable generated memories (or both): skein context memories enable
skein context refresh
skein session search "deploy aura.ai.pro.br"
# run the returned exact command: codex resume THREAD_ID
```

Human-readable output is the default. Add `--format json` for automation;
streaming operations use `--jsonl`.

## What is available today

- Explicit project registration and optional bounded recursive discovery.
- Incremental Git metadata plus a private bounded README, AGENTS, manifest, and
  documentation index.
- Existing Codex thread discovery and durable project/session relationships.
- Defaults-off generated-memory and approved-root raw-session recall.
- Explainable matching, project cards, and factual daily activity summaries.
- Foreground and reconnectable Codex workers with full-access policy receipts,
  redacted monitoring, steer, interrupt, and recovery reconciliation.
- A fail-closed single-prompt conductor, standalone Ratatui TUI, and 26-tool MCP
  server.
- A distributable Codex skill and plugin manifest in this repository.

Session Skein is early alpha software. Read the privacy and control boundaries
before enabling transcript recall or worker control.

## Documentation

Read the [web documentation](https://session-skein.github.io/session-skein/) or start
at the repository [handbook](docs/index.md). Both render the same canonical Markdown.
Its guided paths cover:

- [installation and Codex setup](INSTALL.md)
- [getting started](docs/getting-started.md)
- [concepts and trust boundaries](docs/concepts.md)
- [codebase map and guided tour](docs/codebase-map.md)
- [indexing, search, and network disks](docs/indexing-and-search.md)
- [complete CLI reference](docs/cli-reference.md)
- [MCP workflows](docs/mcp.md) and [all MCP tools](docs/mcp-reference.md)
- [state and configuration](docs/state-and-configuration.md)
- [maintenance](docs/maintenance.md) and [troubleshooting](docs/troubleshooting.md)
- [architecture](docs/architecture.md), [privacy](docs/privacy.md), and
  [roadmap](ROADMAP.md)

The handbook follows a teaching-map approach inspired by Understand Anything:
purpose first, then system layers, end-to-end flows, a source map, and progressively
deeper reference. The maintained Markdown remains useful without a graph viewer or
an LLM.

## Development

Rust 1.95 or newer and a native C compiler/linker are required; the repository pins
Rust 1.95.0. See the platform-specific [installation prerequisites](INSTALL.md#prerequisites).

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release --locked
cargo audit --deny warnings
```

Read [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md) before changing
state, integration, installer, or documentation behavior.

## Independence

Session Skein is an independent MIT-licensed project. It is not affiliated with or
endorsed by OpenAI. Codex and OpenAI are trademarks of their respective owners.
