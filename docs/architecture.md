# Architecture

Session Skein is a local control plane. Its core model is intentionally independent
from any one agent, terminal multiplexer, or subscription provider.

```text
TUI / CLI / MCP
      |
Conductor and policy
      |
Project + session + activity model
      |
Codex | Agent Deck | tmux | Git adapters
      |
Versioned SQLite state and source-owned transcripts
```

## Workspace boundaries

- `skein-core` owns paths, migrations, and domain state. It does not spawn agents.
- `session-skein` produces the `skein` CLI and turns core operations into stable
  human-readable or JSON output.
- Future adapters will read source data incrementally and preserve provenance.
- Future control operations will be separate from observation and require an
  explicit policy decision.

## Git metadata adapter

The first source adapter observes only explicitly registered project roots. It stores
branch, head object, latest commit timestamp and subject, and an optional tracked-file
dirty result. A fingerprint of small Git administrative files lets the default path
avoid spawning Git when repository metadata has not changed. Working-tree checks are
opt-in and exclude untracked files and submodules.

The adapter invokes read-only local Git commands. It does not fetch, contact remotes,
change the index, or discover repositories. See [git-refresh.md](git-refresh.md) for
the observable contract.

## Codex adapter

`skein-codex` is isolated from the core state model. It launches the locally installed
Codex app-server over stdio, performs the documented initialize handshake, and makes
one bounded `thread/list` request. It does not parse Codex's private rollout JSONL
format and does not persist preview results.

The app-server schema is specific to the installed Codex version and may evolve. The
adapter decodes only the fields it exposes publicly and fails closed on protocol or
JSON-RPC errors. See [codex-preview.md](codex-preview.md).

## Performance model

The hot path must not scan repositories. Adapters record cursors and fingerprints,
then refresh only changed sources. SQLite queries serve interactive views. Slow or
remote project roots are polled only when configured, and Git inspection is bounded.

Rust reduces process startup, memory overhead, and deployment friction. It cannot
remove latency caused by a network filesystem or slow physical disk, so the design
avoids that I/O rather than relying on language speed to hide it.

## Compatibility

Source adapters are subordinate compatibility modules. Product identity and core
types do not use another product's name. The project will prefer documented formats
and commands, tolerate missing tools, and surface adapter versions in diagnostics.
