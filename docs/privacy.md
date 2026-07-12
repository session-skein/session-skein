# Privacy and data handling

Session Skein is local-first, but local data can still be sensitive. Session text,
repository paths, branch names, prompts, diffs, and activity timestamps may reveal
personal or proprietary information.

## Rules

- Private generated state belongs in the platform data directory, never the repo.
- On Unix, state directories are mode `0700` and the SQLite file is mode `0600`.
- Default metadata and control paths do not intentionally store credentials or bearer
  tokens. Explicit document/deep-recall sources are stored verbatim within their
  bounds and can contain secrets if the admitted source text contains them.
- Project-document recall is explicit through registered projects and `index`; it
  stores bounded identity-file text privately and never follows symlinks.
- Raw transcripts remain source-owned unless a user explicitly enables bounded
  user/assistant recall beneath an approved root. The observed cwd must be an
  existing directory and both cwd and root are canonicalized, rejecting missing
  paths, non-directories, and symlink escapes.
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
- Steer text remains only in authenticated IPC and the worker's memory queue. Durable
  state stores its byte count, opaque request ID, exact turn ID, and fixed outcome.
- Source reads decode only thread/turn identity, enum-like status, full-history
  availability, and matches against Skein-generated client IDs. Raw `thread/read`
  responses, unrecognized source client IDs, item content, names, and previews are not
  additionally persisted. Skein's own opaque client IDs already exist in the audit
  ledger so acceptance can be correlated without text.
- Project-document indexing stores selected identity-file text. Its fixed-path
  fallback may admit an untracked identity file when Git enumeration fails.
- Generated-memory and raw-session recall are separate defaults-off gates. Admitted
  text is not comprehensively secret-redacted: it may include pasted credentials,
  prompts, commands, diffs, or agent prose present inside a memory or user/assistant
  message. Storage is bounded and owner-private; disabling a source and refreshing
  deletes that source's indexed rows.
- Raw-session incremental checkpoints add only bounded source byte length beside the
  existing private whole-file fingerprint. They contain no transcript excerpt or
  parsed message content, and the prior prefix is fully rehashed before tail reuse.
- Generated-memory project attribution uses conservative path/cwd evidence and the
  longest registered-project match. Conflicting references remain unmapped, and
  memory text never grants raw-session authorization.

Session Skein performs no telemetry and has no external network client. Its worker IPC
is IPv4 loopback-only, its app-server transport is local, and it stores no Codex
credentials. A controlled Codex process does
contact OpenAI and may contact configured web or MCP services under Codex's own
configuration and the explicitly acknowledged full-access policy.

Matching queries are accepted only on stdin, bounded to 64 KiB, used in memory, and
never returned in JSON or persisted. Default matching ignores stored session names and
previews. `--include-text` permits explicitly imported text to affect local scores but
still emits only field names, counts, points, and opaque identities—not the matched
source values. Project cards and day summaries are generated on read and are not
cached. Project cards may display the already stored Git commit subject; neither view
reads session transcripts, agent output, commands, diffs, or MCP content.

Quick project search does not query private context rows. Explicit deep search reports
gates, freshness, bounds, and possible truncation before returning bounded private
snippets. Diagnostics and ambiguous-route evidence contain no transcript excerpt.
Current per-source gates are enforced by every private-context query. Turning a gate
off revokes search access immediately even if rows remain stored until reconciliation;
turning it back on can expose retained owner-private rows with their prior freshness.

The conductor uses the same bounded stdin bytes first as an in-memory route query and
then unchanged as the Codex prompt. Accepted routes persist only request/run IDs,
selected project and optional opaque thread identity, confidence/score/margin, query
byte/token counts, explicit text-consent state, and structured evidence
families/kinds/counts/points. They persist no query, tokens, matched values, candidate
list, prompt hash, or prose. Reusing a request UUID is a status lookup and never
replays prompt content.

The TUI composer keeps prompt text in process memory only. A one-shot full-access
acknowledgement is consumed and the visible composer is cleared before its conductor
child starts. The prompt crosses only the child's stdin; it is absent from argv,
environment variables, SQLite, and Session Skein logs. The child response is bounded
to 1 MiB, and live worker views expose only the existing redacted event schema. These
rules prevent intentional persistence but do not promise secure erasure of operating
system or allocator memory.

MCP arguments and results cross the Codex-owned stdio connection in memory. Session
Skein bounds each argument envelope to 128 KiB, individual text fields to 64 KiB,
and each child output stream to 1 MiB. Project search
does not echo or persist the query. `conduct` and `steer_run` pass prompt text only to
the existing private stdin/IPC paths and return content-free IDs, evidence, and state.
Tool errors are sanitized and bounded. The server exposes content-free session
metadata and redaction-safe control records by default. `refresh_activity` never reads
raw rollout transcripts. `refresh_index` may read enabled context sources, and
`search_projects(include_deep_context=true)` may return their snippets into Codex's
model context. Codex may send that context to OpenAI or another configured provider;
Session Skein itself performs no such network request.

Scoped `refresh_index` calls never broaden filesystem authority: an exact project
selector performs no traversal, and a scan-root selector traverses only the matching
configured policy. Both selectors are validated before traversal. Context and session
sources retain their global atomic replacement boundary and are deferred during a
scoped refresh rather than partially replaced.

The legacy `set_codex_memory_indexing` and `set_codex_session_indexing` tool names map
to durable explicit gates. Both are disabled by default. Raw-session recall requires
an approved scan root and admits only bounded user/assistant message text; disabling
a source and refreshing deletes that source's private index rows atomically.

Project-document search is a different, explicit source. It admits only selected
identity files, caps each file at 64 KiB and each project at 512 KiB, and returns at
most a 2 KiB FTS snippet rather than the stored document body.
