# Changelog

All notable changes will be documented here. The project follows Semantic Versioning
after its first published release.

## [0.5.0-alpha.3] - Unreleased steer and source reconciliation

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
