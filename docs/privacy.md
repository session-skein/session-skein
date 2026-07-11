# Privacy and data handling

Session Skein is local-first, but local data can still be sensitive. Session text,
repository paths, branch names, prompts, diffs, and activity timestamps may reveal
personal or proprietary information.

## Rules

- Private generated state belongs in the platform data directory, never the repo.
- On Unix, state directories are mode `0700` and the SQLite file is mode `0600`.
- Credentials and bearer tokens must not be stored in SQLite or logs.
- Raw transcripts remain source-owned unless a user explicitly imports them.
- Exports are opt-in, inspectable, and redacted before publication.
- Diagnostics must describe locations and health without dumping content.
- Tests use synthetic names and temporary paths.
- Git snapshots store the registered path, branch, object ID, latest commit timestamp
  and subject, and an optional tracked-dirty result. Commit bodies and diffs are not
  stored.
- Codex discovery is dry-run and stores nothing. Thread names and first-message text
  are redacted from output unless `--include-text` is explicitly requested.
- Codex session sync stores opaque thread and session identifiers, cwd, timestamps,
  status/source labels, provider and CLI version, parent/fork identifiers, ephemeral
  state, observation timestamps, and project-link evidence. It stores no turns,
  command output, diffs, MCP payloads, credentials, or rollout paths.
- Thread names and first-message previews are absent from durable state by default.
  `session sync codex --include-text` is the explicit opt-in for storing only those
  two text fields in the private database.
- Control prompts, agent messages, command arguments and
  output, diffs, approval bodies, and MCP payloads remain Codex-owned. Control state
  stores only byte counts, opaque correlation IDs, policy, timestamps, method names,
  fixed sanitized error classifications, source result IDs, and state transitions.
- `control codex --include-content` affects only that command's live output. It does
  not enable persistence.
- Worker prompts cross an authenticated loopback connection only in memory. A random
  capability lives in a mode-`0600` file beneath the private data directory; it is
  absent from SQLite, argv, environment variables, logs, and command output.
- Reconnectable worker events are redacted and retained only in a bounded memory
  window. Historical agent text is not persisted or replayed after a worker exits.

Session Skein performs no telemetry and has no external network client. Its worker IPC
is IPv4 loopback-only, its app-server transport is local, and it stores no Codex
credentials. A controlled Codex process does
contact OpenAI and may contact configured web or MCP services under Codex's own
configuration and the explicitly acknowledged full-access policy.
