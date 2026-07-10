# Changelog

All notable changes will be documented here. The project follows Semantic Versioning
after its first published release.

## [0.2.0] - Unreleased

- Created the clean Rust workspace and independent Session Skein identity.
- Added secure platform state paths and a versioned SQLite project registry.
- Added `doctor`, `init`, `project add`, and `project list` CLI commands.
- Added privacy, security, architecture, contribution, CI, and roadmap baselines.
- Raised the minimum supported Rust version to 1.95.
- Added schema-v2 project metadata with in-place schema-v1 migration.
- Added fingerprinted Git refresh, explicit tracked-file checks, `project show`, and
  explicit single-project or `--all` refresh scopes.
