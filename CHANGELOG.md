# Changelog

All notable changes will be documented here. The project follows Semantic Versioning
after its first published release.

## [0.5.0-alpha.9] - Unreleased

### Added

- Added durable cursor-based worker observation with bounded wait, cancellation
  phases, lease/heartbeat health, pending-action diagnostics, and truthful idempotent
  interrupt reports across CLI and MCP.
- Made every observation poll strictly read-only; expired leases are reported with
  explicit recovery guidance and are never recovered as a side effect of monitoring.

- Added actionable ambiguous-route reports with stable ranked project/session
  selectors, transactionally revalidated explicit resolution, and quick-versus-private
  recall diagnostics across CLI JSON and MCP.
- Enforced live per-source recall gates in every private-context query, making
  revocation immediate before retained rows are reconciled by refresh.

- Added schema-11 content-free byte checkpoints for verified unchanged reuse and
  append-tail parsing of explicitly enabled raw-session context files, with safe full
  fallback and structured work accounting.
- Added the receipt-gated, CLI-only `skein update` workflow with check-only status,
  exact-version selection, downgrade/reinstall policy, and transactional delegation
  to the verified binary-first installer snapshot. Current-version decisions use the
  compiled binary version, and installer snapshot replacement participates in
  rollback with the binary, skill, MCP registration, and receipt.

## [0.5.0-alpha.8] - 2026-07-12

### Added

- Added a PR-tested, tag-published preview release pipeline for native Linux x86_64,
  macOS x86_64/arm64, and Windows x86_64 packages with deterministic archives,
  machine-readable manifests, SHA-256 checksums, and GitHub provenance attestations.
- Added binary-first Linux/macOS and Windows installers with explicit preview-channel
  or exact-version resolution, canonical release URLs, checksum/manifest enforcement,
  traversal-safe extraction, bundled-skill installation, and receipt-safe reinstall.

## [0.5.0-alpha.7]

- Added an on-demand stdio MCP server with server instructions, JSON Schema tools,
  structured results, and read/write/destructive/open-world annotations.
- Preserved Codex Brain project-recall and activity tool names while mapping roots to
  explicit projects and keeping sensitive memory/session ingestion opt-in.
- Exposed content-free project/session/run views plus audited conductor, steer,
  interrupt, and reconciliation operations to Codex CLI.
- Added separately persisted exact or opt-in recursive scan roots, bounded discovery,
  worktree recognition, provenance, unplugged-root diagnostics, and schema 8 migration.
- Made command output human-readable by default with global `--format json` support;
  retained legacy `--json` and streaming `--jsonl` compatibility.
- Added complete bounded Codex session pagination on one app-server connection, with
  repeated-cursor detection and no partial import after later-page failure.
- Added schema-9 private FTS5 recall for bounded project identity files (Git-tracked
  when available, fixed bounded fallback otherwise), fingerprinted refresh, bounded
  snippets, CLI search, and MCP document matches.
- Added schema-10 opt-in Codex memory and approved-root session-message recall with
  atomic rebuilds, private FTS, strict source/text/file bounds, and safe defaults.
- Unified repository discovery, resilient per-project refresh, context recall, and
  complete content-free Codex session synchronization behind `skein index` and TUI
  startup; unavailable network projects no longer abort the remaining refresh.
- Added fresh-install MCP initialization and setup guidance, caller-owned control
  request IDs, explicit per-query deep-context disclosure, and privacy documentation
  for admitted text.
- Made deep-recall source budgets independent and non-destructive: truncated sources,
  missing Codex directories, and unavailable approved roots retain prior private rows
  and memory-to-project routing until an authoritative refresh is possible.
- Stop recursive discovery at Git repository boundaries, matching Codex Brain's fast
  scan behavior and avoiding expensive traversal inside network-hosted source trees.
- Added an organized handbook with a guided codebase map, complete CLI and 24-tool
  MCP references, installation/operations guides, and automated drift checks.
- Added idempotent Linux/macOS and Windows installers, a distributable Codex skill,
  and a repository marketplace plugin with MCP configuration. Direct installs use
  immutable content-addressed skill snapshots plus receipt-, binary-, and MCP-config
  rollback so failed upgrades cannot change the active integration.
- Corrected stale pre-conductor architecture, TUI, worker, schema, pagination, and
  security documentation.

## [0.5.0-alpha.6] - 2026-07-11

- Added a keyboard-first terminal interface with a project library, project-scoped
  session/run navigation, deterministic daily cards, and one global conductor input.
- Added one-shot full-access arming, bounded child protocol handling, stable request
  reconciliation, exact-run interrupt confirmation, and redacted live worker events.
- Added pinned actionable blockers for recovery-required and failed runs plus
  non-resumable session states.
- Kept all catalog I/O and worker operations off the rendering thread and retained a
  complete Codex CLI-only path with no tmux, Agent Deck, MCP, or service dependency.

## [0.5.0-alpha.5] - 2026-07-11

- Added one private-stdin `conduct` entry point that dispatches only a unique,
  high-confidence deterministic project/session route after ChatGPT authentication.
- Added schema-7 content-free routing receipts, structured evidence, request UUID
  idempotency, atomic worker allocation, and deterministic pre-dispatch crash failure.
- Hardened automatic resume against ephemeral, system-error, subagent, unbound,
  already-active, and recovery-required Codex threads.

## [0.5.0-alpha.4] - 2026-07-11

- Added private-stdin metadata matching across registered projects and linked sessions
  with deterministic integer evidence, stable confidence labels, and no dispatch.
- Added generated-on-read project cards and local-day activity digests from already
  observed Git, session, and control metadata without Codex, an LLM, or cached prose.

## [0.5.0-alpha.3] - 2026-07-11

- Added idempotent, exact-active-turn steering through reconnectable worker IPC with
  an interrupt barrier and content-free durable audit evidence.
- Added redacted `thread/read` inspection and audited reconciliation of a lost
  worker's exact terminal Codex turn without replay, takeover, or transcript storage.

## [0.5.0-alpha.2] - 2026-07-11

- Added per-run worker ownership, fenced leases, and private authenticated loopback
  IPC so a client can disconnect without owning the Codex process lifetime.
- Added worker discovery, durable status, bounded redacted watch windows with gap
  markers, exact audited interruption, guarded Codex child cleanup, and expired-owner
  quarantine without replay.

## [0.5.0-alpha.1] - 2026-07-11

- Added explicitly targeted foreground Codex thread start/resume and managed turn
  execution through app-server with existing Codex ChatGPT authentication.
- Added immutable full-access policy snapshots, redaction-safe run state, and a
  crash-conservative action/event audit ledger in schema version 4.
- Added redacted JSONL monitoring and authoritative completion from
  `turn/completed`; prompts and outputs remain Codex-owned.

## [0.4.0] - 2026-07-11

- Added a source-neutral durable session catalog with transactional, idempotent Codex
  metadata synchronization.
- Added deterministic project association, explicit bind/unbind overrides, and
  session list/show commands.
- Made Codex CLI the documented first-class runtime and moved Agent Deck and tmux to
  an optional enrichment track.

## [0.3.0] - 2026-07-10

- Added a separate, version-aware `skein-codex` adapter crate.
- Added redacted-by-default `import codex preview` through the documented app-server
  thread API, with bounded pages and explicit source-index repair.

## [0.2.0] - 2026-07-10

- Created the clean Rust workspace and independent Session Skein identity.
- Added secure platform state paths and a versioned SQLite project registry.
- Added `doctor`, `init`, `project add`, and `project list` CLI commands.
- Added privacy, security, architecture, contribution, CI, and roadmap baselines.
- Raised the minimum supported Rust version to 1.95.
- Added schema-v2 project metadata with in-place schema-v1 migration.
- Added fingerprinted Git refresh, explicit tracked-file checks, `project show`, and
  explicit single-project or `--all` refresh scopes.
