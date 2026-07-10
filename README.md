# Session Skein

Session Skein is a fast, local-first conductor for coding-agent projects, sessions,
and activity. It is being built around one durable entry point: describe the work,
let the conductor resolve the right project and session, then inspect or steer the
workers from one place.

The project is in an early foundation phase. Today, the `skein` binary provides a
secure versioned project registry and diagnostics. It does not yet control Codex,
Agent Deck, tmux, or MCP clients.

## Why a skein?

Individual agent sessions are threads. Projects, decisions, worktrees, and history
weave those threads into something larger. Session Skein keeps that structure local,
inspectable, and recoverable without treating one giant prompt as permanent memory.

## Current commands

```console
cargo run --release --bin skein -- doctor
cargo run --release --bin skein -- init
cargo run --release --bin skein -- project add /path/to/project
cargo run --release --bin skein -- project list --json
```

`doctor` is read-only when no database exists. `init` creates a private per-user
SQLite database. Project discovery is explicit in this first release; Session Skein
does not recursively crawl disks or network mounts.

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

## License

MIT. See [LICENSE](LICENSE).
