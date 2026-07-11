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

Session Skein currently performs no telemetry and no network requests at runtime.
That invariant must remain documented if a future optional integration changes it.
