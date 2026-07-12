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
its `session_meta.cwd` is an absolute descendant of a persisted approved scan root.
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

Each enabled source is rebuilt atomically with its FTS rows. Settings and approved
roots are revalidated after the write lock is acquired. Disabling a source changes
only its gate; the next `context refresh` or `index` atomically removes that source's
previous documents.

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
