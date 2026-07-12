# CLI fallback

## Installation health

```console
command -v skein
skein --version
skein doctor
codex login status
codex mcp get session-skein --json
```

If the repository is present, follow its root `INSTALL.md`. Use an absolute installed
binary in direct MCP configuration.

`skein update --check --json` is read-only. Run mutating `skein update` only when the
user asks to update an unchanged release-owned installation. It is intentionally not
an MCP tool; source installations use the documented source-installer flow.

## Setup and index

```console
skein init
skein project add /explicit/repository
skein scan-root add /explicit/workspace --recursive --max-depth 16
skein index
skein session sync codex --all-pages
```

Choose either exact registration or an approved root according to the user's scope;
do not blindly run both examples.

## Search and inspect

```console
skein search distinctive terms
skein search --deep-context distinctive terms  # explicit private recall only
skein session list --project /registered/project
skein worker list --active
skein summary project /registered/project
skein summary day
```

Use `--format json` for parsing.
MCP-only arguments are not automatically CLI flags: `list_sessions` has a `limit`
field, but `skein session list` has no `--limit` option. Check generated `--help`
before translating another tool call.

## Conduct and recover

```console
printf '%s\n' 'private prompt' | \
  skein conduct --full-access --request-id UUID --follow

skein worker status RUN_ID
skein worker observe RUN_ID --after-cursor 0 --json
skein worker read RUN_ID
skein worker reconcile RUN_ID --request-id UUID
```

Prompts and match queries belong on stdin, not argv. Reuse a request UUID only for
status/retry of the same logical operation.
After interrupt, observe from the returned cursor until terminal; queued never means
cancelled.
For ambiguity, preserve the original stdin prompt and use the reported `--project-id`,
plus `--session-id` only for its ranked resumable session.

## Private context

```console
skein context status
skein context memories enable
skein context sessions enable
skein context refresh
skein context search terms
```

Do not run either enable command without the user's informed intent. Disabling a
source is applied to stored documents by the next refresh/index.
