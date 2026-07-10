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
