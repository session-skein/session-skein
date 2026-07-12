# Optional Codex context recall

Session Skein keeps sensitive deep recall disabled by default. Inspect and change the
two independent gates explicitly:

```console
skein context status
skein context memories enable
skein context sessions enable
skein context refresh
skein context search webhook reconciliation
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

Each enabled source independently considers 1,000 files by default and accepts an
explicit maximum up to 10,000. Source files over 1 MiB are skipped; imported text is
capped at 512 KiB per document; titles at 256 bytes; returned FTS snippets at 2 KiB;
search at 100 results. Symlinked files and directories are never followed. Stored
provenance is relative to the selected Codex home.

Candidate files are selected newest-first by modification time, with stable path
ordering for timestamp ties, before the per-source file budget is consumed. Generated
memory files may be attributed to one registered project using conservative path/cwd
metadata. The longest canonical project match wins; conflicting multi-project
references remain unmapped. Memory attribution never authorizes raw-session import.

Each enabled source is reconciled atomically with its FTS rows. Settings and approved
roots are revalidated after the write lock is acquired. Disabling a source changes
only its gate; the next `context refresh` or `index` atomically removes that source's
previous documents.

Raw-session JSONL reconciliation is incrementally parsed after the first full build.
For each admitted file, Skein stores only bounded byte length beside the existing
private whole-file fingerprint. An unchanged repeat fully rereads and hashes the file
to prove equality, but reuses the parsed private document without JSONL processing.
If a file grew and its entire prior prefix matches at a newline boundary, only the
appended records are parsed. The complete bounded candidate set is still enumerated,
so deletion is reconciled atomically.

A missing legacy checkpoint, shrink, rewritten prefix, changed or unauthorized cwd,
or non-newline append falls back to a full bounded parse. Truncated or unavailable
discovery remains deferred. Reports expose `mode` (`full`, `incremental`, `unchanged`,
`fallback_full`, `disabled`, or `deferred`) plus byte, record, reuse, fallback, and
deletion counts. Checkpoints contain no transcript excerpts or parsed message text.

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
