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
- [x] User-approved exact or optional recursive scan roots with bounded exclusions.
- [x] Project metadata, Git state, and incremental refresh.
- [x] Exact project and exact configured-root scoped index refresh.
- [x] Import adapters with dry-run previews.

## Phase 2: session model and local recall

- [x] Read-only Codex session discovery.
- [x] Durable Codex session catalog and project relationships.
- [x] Search and ranking with explainable evidence.
- [x] Cheap incremental activity summaries.
- [x] Bounded README, AGENTS, manifest, and docs full-text index.
- [ ] Optional Git-tracked, symbol-ranked structural map inspired by Aider repo maps.
- [x] Setup-required MCP onboarding on a fresh local state database.
- [ ] Lazy stale-index refresh.
- [x] Configurable generated-memory and approved-root raw-session recall profiles.
- [ ] Incremental session/activity cursors with real time-window semantics.

## Phase 3: Codex-native control

- [x] Explicit project/thread start, resume, turn start, and foreground monitoring.
- [x] Reconnectable detached watch and exact active-turn interruption.
- [x] Separate source read and active-turn steer operations.
- [x] Explicit full-access policy snapshots and audit records for control operations.
- [x] Read-only reconciliation of exact terminal turns after worker loss.
- [ ] Worker takeover or reattachment after worker loss.
- [x] Fenced, per-run Codex process ownership without tmux.

## Phase 4: conductor

- [x] One prompt entry point with explicit routing confidence.
- [x] Policy boundary for full-access workers.
- [x] MCP stdio transport and stable conductor-control protocol for Codex CLI.
- [ ] Complete Codex Brain recall workflow parity through MCP.
- [x] Crash-fenced planning, no-replay audit, and deterministic pre-dispatch failure.
- [ ] Crash-safe job continuation or takeover after worker loss.

## Phase 5: TUI

- [x] Project library and project-scoped session/run navigation.
- [x] One global composer with deterministic conductor routing.
- [ ] Optional project-scoped tabs around the global composer.
- [x] Live worker state and bounded redacted event views.
- [x] Actionable blocker views from durable run and session state.
- [ ] Cost views when reliable source data is available.
- [x] Daily narrative and project-card views.
- [x] Keyboard-first recovery of previous work from durable state.
- [ ] Charm-inspired Ratatui themes, Markdown views, components, and restrained effects.

## Phase 6: narrative and freshness

- [ ] Standalone low-priority source refresh scheduler.
- [ ] Optional user systemd/autostart adapters; never a runtime requirement.
- [ ] Cached project descriptions regenerated only after relevant source changes.
- [ ] Daily work narratives with explicit coverage and provenance.
- [ ] Optional Codex-generated descriptions through the existing ChatGPT login.

## Optional enrichment track

- [ ] tmux observer for pane correlation and attach hints.
- [ ] Agent Deck observer when its executable and registry are available.
- [x] Codex skill and plugin that teach Session Skein workflows without owning state.

## Distribution and documentation

- [x] Agent-readable installation contract with idempotent Linux/macOS and Windows
  setup paths.
- [x] Organized handbook, guided codebase map, complete CLI/MCP references, and
  automated documentation drift checks.
- [x] Codex marketplace manifest that bundles the workflow skill and MCP declaration.
- [x] Unsigned preview archives for Linux x86_64, macOS x86_64/arm64, and Windows
  x86_64 with deterministic packaging, checksums, manifests, and GitHub provenance.
- [ ] Sign and notarize macOS releases and Authenticode-sign Windows releases.
- [ ] Make installers binary-first only after the signed asset/install trust contract
  is designed; do not silently replace the source-first path.
- [ ] Add an explicit `skein update` flow after signed release selection and rollback
  semantics are defined.
- [ ] Package-manager distribution after the public command/config surface stabilizes.

Missing, outdated, or broken optional integrations must degrade to diagnostics and
must not affect Codex discovery, control, routing, database startup, or the TUI.

Non-goals for the first public releases include cloud synchronization, scanning any
unapproved root, and storing API credentials in the state database. Recursive
discovery is supported only for roots the user explicitly marks recursive.

## Scoped-index follow-up backlog

The first scoped-index slice deliberately excludes progress streaming, cancellation,
ranking redesign, and deep-context UX changes. Potential follow-ups are scoped-source
progress events, safe cancellation checkpoints, ranking work measured independently
from traversal, and a separately consented partial deep-context replacement design.
