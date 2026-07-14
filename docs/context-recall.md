# Optional Codex context recall

Session Skein keeps sensitive deep recall disabled by default. Inspect and change the
two independent gates explicitly:

```console
skein context status
skein context memories enable
skein context sessions enable
skein context refresh
skein context search webhook reconciliation
skein session search "deploy aura.ai.pro.br"
skein search webhook reconciliation
skein search --deep-context webhook reconciliation
```

`CODEX_HOME` selects the source directory, defaulting to `~/.codex`; a one-shot
`--codex-home` override is available for refreshes and tests. The normal `skein index`
and TUI startup refresh apply enabled sources automatically.

Generated-memory recall considers regular Markdown files beneath `memories/`.
Raw-session recall considers JSONL beneath `sessions/`, but imports a file only when
its observed cwd resolves to an existing directory whose canonical path is beneath a
canonical persisted approved scan root. Missing paths, non-directories, and symlink
escapes are rejected.
Only `response_item` messages with user or assistant roles contribute text. System,
developer, tool, approval, command, diff, and malformed records are ignored.
Admitted user/assistant or generated-memory text is not comprehensively redacted and
may itself contain pasted secrets, commands, diffs, prompts, or agent prose. Treat the
private index as sensitive local data.

Each enabled source independently considers 10,000 files by default and at most.
Generated-memory files over 1 MiB are skipped. Session JSONL is streamed to EOF
regardless of file size: individual records are capped at 1 MiB and the retained
early-plus-recent projection remains capped at 512 KiB per session. Titles are capped
at 256 bytes, returned FTS snippets at 2 KiB, and search at 100 results. Symlinked
files and directories are never followed. Stored provenance is relative to the
selected Codex home.

Candidate files are selected newest-first by modification time, with stable path
ordering for timestamp ties, before the per-source file budget is consumed. Generated
memory files may be attributed to one registered project using conservative path/cwd
metadata. The longest canonical project match wins; conflicting multi-project
references remain unmapped. Memory attribution never authorizes raw-session import.

Each enabled source is reconciled atomically with its FTS rows. Settings and approved
roots are revalidated after the write lock is acquired. Disabling a source changes
only its gate; the next `context refresh` or `index` atomically removes that source's
previous documents.

The gate also applies at query time. Disabling a source immediately makes retained
rows non-searchable; no refresh is required for revocation. Re-enabling may make
those owner-private rows searchable again before refresh, with their existing
freshness timestamps. Refresh when current source coverage is required.

Raw-session JSONL reconciliation is incremental after the first full build. Schema 12
stores size/mtime checkpoint metadata plus exact `session_meta` thread identity and
source event timestamps. A matching checkpoint makes an unchanged repeat metadata-
only. If a file grew and a streaming hash proves its entire prior prefix at a newline
boundary, only appended records are parsed. The complete candidate set is still
enumerated, so deletion is reconciled atomically.

A missing or inconsistent checkpoint, shrink, rewritten prefix, changed thread/cwd,
unauthorized cwd, or non-newline append falls back to a full streaming parse. Injected
AGENTS/environment startup payloads are excluded from titles and search text.
Truncated or unavailable
discovery remains deferred. Reports expose `mode` (`full`, `incremental`, `unchanged`,
`fallback_full`, `disabled`, or `deferred`) plus byte, record, reuse, fallback, and
deletion counts. Checkpoints contain no transcript excerpts or parsed message text.
For sessions with no admitted checkpoint, Skein reads only a bounded early metadata
preflight first. An early `session_meta` cwd that is definitely outside every approved
root—or is a stale/deleted directory beneath a reachable approved root—stops that file
immediately; its transcript body is neither parsed nor retained.

`session search` is the resumable private-search path. It consults enabled raw-session
projections plus generated files under `memories/rollout_summaries/` that carry one
strictly validated `thread_id`; aggregate memory files never gain a resumable identity.
This lets a summary identify a session whose original cwd is outside approved raw-
transcript roots without broadening those roots. It requires every normalized
distinctive term first, ignores conversational filler, and treats longer terms as
prefixes so “deploy” can match “deployed”. It uses an any-term fallback only when
strict search has no hits. Duplicate sources for one thread collapse to one hit.
Results include source provenance, exact thread ID, relative source path, source dates,
optional cwd/project, bounded title/snippet, rank/match mode, and
`codex resume THREAD_ID`. Search alone is read-only; `--refresh` explicitly refreshes
enabled sources first.

Interactive `context refresh` reports its scan and completion stages on stderr.
Progress is suppressed for JSON and non-TTY use. `skein freshness` reads the latest
durable context observation without opening source directories; an empty context
source is expected while both privacy gates remain disabled.

For an enabled source, `unchanged` means the complete bounded source was observed and
its fingerprints matched; all retained rows receive the new observation timestamp
without an FTS rebuild. Deferred unavailable or truncated sources retain both their
content and prior timestamps. Freshness uses the oldest row in the source.

A refresh that cannot authoritatively observe an enabled source—for example, because
its directory or an approved network root is unavailable—retains that source's prior
rows and reports `deferred_unavailable`. A source that exceeds its file cap likewise
retains prior rows and reports `deferred_truncated`, avoiding destructive partial
rebuilds. Explicitly disabling a source remains authoritative and removes its rows on
the next refresh.

MCP search does not expose deep-context snippets by default. A caller must also set
`include_deep_context=true`; those snippets then enter Codex's model context and may
be sent by Codex to OpenAI or another configured provider. Session Skein has no
network client of its own.

General CLI and MCP search returns `recall` diagnostics: quick versus private mode,
exact consulted source families derived from enabled gates, conservative freshness,
result limits, possible truncation, and the explicit escalation path. Quick mode never
queries private context rows. Deep mode searches only already authorized and indexed
rows; source enablement and refresh remain separate explicit operations.
