# Durable Codex session synchronization

Session Skein works directly with the locally installed Codex CLI. Agent Deck, tmux,
MCP, a background service, and a separate API key are not required.

Preview remains the write-free consent boundary:

```console
skein import codex preview --limit 50 --json
```

Synchronize one bounded page only after inspecting that shape:

```console
skein session sync codex --limit 50 --json
```

For the normal “import everything currently cataloged by Codex” workflow, follow
opaque cursors automatically with explicit hard bounds:

```console
skein session sync codex --all-pages --limit 100 \
  --max-pages 100 --max-threads 10000
```

All pages use one initialized app-server connection. Repeated cursors, a later-page
protocol failure, or an invalid bound fails the operation before any partial public
result is imported.

Use `--since-days N` to import only threads whose source `updatedAt` falls inside a
recent window. The app-server page remains bounded and newest-first; this is a real
metadata filter, not a claim that raw transcript activity was inspected.

The synchronization performs the same documented app-server handshake and newest-
first `thread/list` request as preview, then commits the complete page atomically. A
missing or failed Codex process cannot partially update or migrate the database.
An `unchanged` count means the source metadata was unchanged; Session Skein still
refreshes the private `last_seen_at` observation timestamp. Older overlapping pages
cannot replace metadata already observed with a newer Codex `updatedAt` value.

## Stored metadata

Each row retains source-owned thread and session IDs, observed cwd, source creation
and update timestamps, first/last observation timestamps, source and last observed
app-server status labels, model provider, Codex CLI version, parent/fork IDs,
ephemeral state, and project-link evidence. The status belongs to the newly spawned
app-server observation and is not proof that an independent Codex CLI process is
currently running. Session Skein never stores the unstable rollout path or raw turns.

Names and first-message previews are null by default. Use `--include-text` only when
storing those two fields in the private local database is acceptable. A later
redacted sync does not erase text that was imported explicitly.

Stored text is also redacted from later `session list`, `show`, `bind`, and `unbind`
output by default. Pass `--include-text` to the individual output command when that
terminal or JSON consumer may receive it. `text_redacted: true` indicates that stored
text was deliberately omitted.

## Project association

Synchronization never registers projects. When an observed cwd exists, Session Skein
matches registered canonical project roots component by component and selects the
longest ancestor. This handles nested worktrees without confusing paths such as
`app` and `application`. Missing or unmatched paths remain unassigned and visible.

Manual choices survive later synchronization:

```console
skein session bind THREAD_ID /path/to/registered/project --json
skein session unbind THREAD_ID --json
```

`unbind` is an explicit decision, not a request to immediately run automatic matching
again.

## Pagination and compatibility

`--limit` is bounded to 1 through 1000. `nextCursor` may be passed back with
`--cursor` during the current scan. Cursors are opaque Codex values and should not be
treated as permanent checkpoints across Codex upgrades or long gaps.

The default sends `useStateDbOnly: true`. `--repair-source-index` separately opts into
Codex's rollout scan-and-repair path. Additive app-server fields are ignored, unknown
source/status labels remain representable, and no raw JSONL fallback is attempted.
