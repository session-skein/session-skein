# Roadmap

The order is intentional: durable local state first, adapters second, orchestration
third, and a polished TUI only after the underlying behavior is observable and safe.

## Phase 1: local foundation

- [x] Clean Rust workspace and public repository hygiene.
- [x] Private, versioned SQLite state.
- [x] Explicit project registration and diagnostics.
- [x] Project metadata, Git state, and incremental refresh.
- [x] Import adapters with dry-run previews.

## Phase 2: session model

- [x] Read-only Codex session discovery.
- [ ] Agent Deck and tmux session adapters.
- [ ] Durable project-session-task relationships.
- [ ] Search and ranking with explainable evidence.
- [ ] Cheap incremental activity summaries.

## Phase 3: conductor

- [ ] One prompt entry point with explicit routing confidence.
- [ ] Launch, resume, steer, interrupt, and monitor operations.
- [ ] Policy boundary for full-access workers.
- [ ] MCP server and stable machine-readable protocol.
- [ ] Crash-safe job execution and audit trail.

## Phase 4: TUI

- [ ] Project library and session tree.
- [ ] One global composer plus project-scoped tabs.
- [ ] Live worker status, output, blockers, and costs.
- [ ] Daily narrative and project-card views.
- [ ] Keyboard-first recovery of previous work.

Non-goals for the first public releases include cloud synchronization, silent
recursive filesystem crawling, and storing API credentials in the state database.
