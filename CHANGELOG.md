# Changelog

All notable changes will be documented here. The project follows Semantic Versioning
after its first published release.

## [0.4.0] - Unreleased

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
