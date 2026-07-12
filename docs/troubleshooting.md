# Troubleshooting

Start with live facts:

```console
skein doctor
skein context status
codex --version
codex login status
codex mcp get session-skein --json
```

## `skein` is not found

Open a new shell, then inspect `PATH` and the install receipt reported by the
installer. Common user binary directories are not inherited by every GUI launch.
The direct MCP profile uses an absolute executable path and therefore avoids that
problem. The plugin MCP profile invokes `skein` through `PATH`.

Re-run the installer with an explicit destination already visible to Codex:

```console
./install.sh --bin-dir /path/already/on/PATH --control
```

## Codex MCP does not start

1. Verify the configured command is absolute and executable:
   `codex mcp get session-skein --json`.
2. Run that binary's `--version` directly.
3. Do **not** test `skein mcp` in an interactive terminal; it correctly refuses
   because MCP requires stdio protocol input.
4. Remove and re-add a stale registration with the installer `--replace-mcp` option.
5. Start a new Codex session and use `/mcp`.

If both the direct MCP registration and plugin-provided server are enabled, disable
one to avoid duplicate tool namespaces.

## Codex authentication or app-server fails

Run:

```console
codex login status
skein import codex preview --limit 1
```

Session Skein uses the installed CLI's ChatGPT login and does not accept or store an
API key. If the app-server protocol changed, capture the redaction-safe error and
compare the installed Codex version with supported CI/tests. `SKEIN_CODEX_BIN` can
select another local Codex executable for diagnosis.

## No projects appear

```console
skein scan-root list
skein project list
skein index
```

An exact scan root checks only that directory; add `--recursive` for nested Git
repositories. A plain directory without `.git` is not discovered as a project, but
can still be registered explicitly with `project add` if that is intentional.

## Recursive indexing is slow

The first walk can be dominated by disk/network metadata, especially across WSL and
a network-mounted physical disk. Reduce the approved root, use targeted roots, keep
depth bounded, and avoid `--force` and `--working-tree` during routine refreshes.
Repository boundaries already prevent walking entire source trees.

See [network workspace guidance](indexing-and-search.md#slow-and-network-mounted-workspaces).

## A network root is unavailable

This is a deferred source, not a reason to delete cached projects. Mount it and rerun
`skein index`, or remove its stored authorization while offline:

```console
skein scan-root remove /stored/root/path
```

## Existing Codex sessions are missing

```console
skein import codex preview --limit 50
skein session sync codex --all-pages
skein session list --unmatched
```

Use `--repair-source-index` only when you explicitly accept Codex's slower rollout
scan-and-repair path. Unmatched cwd metadata never silently creates a project. Bind a
known session explicitly when needed.

## Search does not find private session content

That is the default privacy behavior. Check:

```console
skein context status
```

Generated memories and raw sessions have separate opt-ins. Raw sessions additionally
need an existing canonical cwd beneath an approved scan root. After changing policy,
run `skein context refresh` or `skein index`. MCP search also requires
`include_deep_context=true` on the individual call.

## Conductor refuses to route

Use the decision-only path:

```console
printf '%s\n' 'the same request' | skein match
```

Inspect evidence and improve project identity docs, use a more specific prompt, or
bind the relevant session. Do not treat ambiguity as an error to bypass; refusal is
the conductor's safety contract.

## Worker is lost or `recovery_required`

```console
skein worker status RUN_ID
skein worker read RUN_ID
skein worker reconcile RUN_ID
```

Reconciliation reads the exact recorded source turn. A terminal turn can close the
run; an in-progress or missing turn remains recovery-required. Skein never replays the
private prompt or automatically takes over work.

## Worker will not stop

`worker stop` intentionally refuses an active run. Interrupt the exact turn if that
is the desired user action, wait for terminal state, then stop the idle worker.

## Schema or database error

`doctor` shows the observed schema without migrating. Back up the complete data
directory, then run the current `skein init`. Do not downgrade blindly after a
migration. See [maintenance](maintenance.md).

## TUI seems stale while starting

The TUI renders cached SQLite state first and refreshes scan roots, Git, identity
docs, enabled context, and Codex session metadata in a background worker. Slow or
offline roots may keep the startup indicator active while navigation remains usable.
Check the surfaced blocker/report instead of restarting repeatedly.

## Report a defect

Follow [SECURITY.md](../SECURITY.md) for vulnerabilities. For normal issues, include
versions, the redaction-safe command/error, and synthetic paths. Never attach the
database, transcripts, credentials, private repository paths, or capability files.
