# Roadmap

Codex CLI is the primary runtime and the standalone path must remain complete.
Optional terminal and session tools may enrich observation and navigation, but cannot
be prerequisites for discovery, control, routing, persistence, or the TUI. The order
is intentional: durable state first, native Codex control second, orchestration third,
and a polished TUI only after behavior is observable and safe.

## Phase 1: local foundation

- [x] Clean Rust workspace and public repository hygiene.
- [x] Private, versioned SQLite state.
- [x] Explicit project registration and diagnostics.
- [x] Project metadata, Git state, and incremental refresh.
- [x] Import adapters with dry-run previews.

## Phase 2: session model

- [x] Read-only Codex session discovery.
- [x] Durable Codex session catalog and project relationships.
- [ ] Search and ranking with explainable evidence.
- [ ] Cheap incremental activity summaries.

## Phase 3: Codex-native control

- [x] Explicit project/thread start, resume, turn start, and foreground monitoring.
- [x] Reconnectable detached watch and exact active-turn interruption.
- [ ] Separate source read and active-turn steer operations.
- [x] Explicit full-access policy snapshots and audit records for control operations.
- [ ] Read-only reconciliation of uncertain runs after restart.
- [x] Fenced, per-run Codex process ownership without tmux.

## Phase 4: conductor

- [ ] One prompt entry point with explicit routing confidence.
- [ ] Policy boundary for full-access workers.
- [ ] MCP server and stable machine-readable protocol.
- [ ] Crash-safe job execution and audit trail.

## Phase 5: TUI

- [ ] Project library and session tree.
- [ ] One global composer plus project-scoped tabs.
- [ ] Live worker status, output, blockers, and costs.
- [ ] Daily narrative and project-card views.
- [ ] Keyboard-first recovery of previous work.

## Optional enrichment track

- [ ] tmux observer for pane correlation and attach hints.
- [ ] Agent Deck observer when its executable and registry are available.
- [ ] Optional skill that teaches Session Skein workflows without owning state.

Missing, outdated, or broken optional integrations must degrade to diagnostics and
must not affect Codex discovery, control, routing, database startup, or the TUI.

Non-goals for the first public releases include cloud synchronization, silent
recursive filesystem crawling, and storing API credentials in the state database.
