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

Session Skein currently performs no telemetry and no network requests at runtime.
That invariant must remain documented if a future optional integration changes it.
