# Codex session preview

Session Skein discovers Codex threads through `codex app-server`, the documented local
interface used for conversation history and rich clients. It does not depend on the
internal layout of files under `$CODEX_HOME/sessions`.

```console
skein import codex preview --limit 50 --json
```

The command performs the app-server initialize handshake, requests one newest-first
`thread/list` page, prints the result, and terminates the child process. It records no
Session Skein state. A 15-second watchdog terminates a child app-server that does not
complete the preview.

## Redaction

By default, results include identifiers, session-tree relationships, cwd, timestamps,
source, runtime status, provider, creator CLI version, and ephemeral state. Thread
names and first-message previews are returned as `null`, with `textRedacted: true`.

Use `--include-text` only when displaying those fields in the current terminal is
acceptable:

```console
skein import codex preview --limit 20 --include-text --json
```

## Cost controls

The page limit must be between 1 and 1000. The default is 50. Pass the opaque
`nextCursor` back through `--cursor` to preview the next page.

Session Skein sends `useStateDbOnly: true` by default, preventing Codex from scanning
rollout JSONL files to repair its index. `--repair-source-index` explicitly permits
that slower Codex-owned scan when the state database appears incomplete.

## Compatibility

App-server schemas are generated for a particular Codex version and may evolve. The
adapter uses stable JSON-RPC response IDs, ignores unrelated notifications, extracts
only documented thread-list fields, and reports protocol mismatches rather than
falling back to raw transcript parsing.

Set `SKEIN_CODEX_BIN` to an alternate Codex executable path when testing another
installed version. The executable continues to use its own existing authentication;
Session Skein does not read or store Codex credentials.
