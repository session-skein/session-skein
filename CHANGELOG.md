# Changelog

All notable changes will be documented here. The project follows Semantic Versioning
after its first published release.

## [0.5.0-alpha.6] - Unreleased standalone conductor TUI

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
