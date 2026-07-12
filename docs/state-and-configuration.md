# State and configuration

Session Skein uses platform-native per-user directories. `skein doctor` is the
authoritative way to see the paths on the current machine.

```console
skein doctor
skein --format json doctor
```

## Path layout

The data directory contains:

```text
session-skein/
├── skein.sqlite3
├── skein.sqlite3-wal        # may exist while WAL is active
├── skein.sqlite3-shm        # may exist while WAL is active
└── workers/
    └── run-RUN_ID.capability
```

The config directory is reserved for future user configuration. The current release
does not consume a Session Skein config file. MCP registration belongs to Codex's
configuration, not this directory.

The installers keep a separate ownership receipt under the platform's user state
area (`~/.local/state/session-skein/install` on typical Unix systems and
`%LOCALAPPDATA%\SessionSkein\install` on Windows). It contains paths, hashes, source
revision, and integration ownership—not project/session content. It exists so update
and uninstall can preserve user-replaced files. The same private installer area holds
content-addressed copies of released skill instructions; the active Codex skill link
points to one of these immutable snapshots instead of the mutable Git checkout.

Typical platform locations are selected by the operating system's standard data and
config conventions. Do not script a guessed path; parse `doctor --format json` or use
the environment overrides below.

## Public environment variables

| Variable | Effect |
| --- | --- |
| `SKEIN_CONFIG_DIR` | Override Session Skein's complete config directory |
| `SKEIN_DATA_DIR` | Override its complete local data directory |
| `SKEIN_CODEX_BIN` | Select the Codex executable used by app-server operations |
| `CODEX_HOME` | Select the Codex home used for memory/session source discovery |

Set these in the environment that starts `skein`. An MCP-launched process inherits
only what Codex forwards or sets in its server configuration.

Internal worker-containment and test variables are deliberately not a supported user
configuration API.

## Database behavior

- The database is SQLite with forward-only versioned migrations.
- Owner-private permissions are applied to state paths where the platform supports
  them.
- Normal writable opens may migrate an older schema. `doctor` is the exception: it is
  strictly read-only and reports the observed version.
- WAL sidecar files may contain recent state and must be included in a live backup.
- A newer schema may not be readable by an older binary. Back up before upgrades when
  rollback matters.

## Worker capability files

Each live reconnectable worker creates an owner-private capability record beneath
`workers/`. It contains the information needed for authenticated local loopback IPC;
it is not a public API token and should not be copied or exposed. The worker removes
its capability during normal shutdown. A stale file is diagnosed through run/worker
state, not blindly reused.

Do not delete the database, WAL files, or a capability file while a worker is active.

## Codex-owned data

The normal Codex home is `~/.codex`, unless `CODEX_HOME` says otherwise. Codex owns:

- authentication and client configuration;
- thread/turn state and rollout files;
- generated memory summaries; and
- MCP/plugin configuration.

Session Skein reads thread metadata through `codex app-server`. Generated memories
and raw session JSONL are read only after separate context opt-ins. It never copies
Codex credentials.

## No required service

There is no required Session Skein daemon, systemd unit, login item, tmux server,
Agent Deck process, HTTP listener, or cloud account. A reconnectable worker is one
on-demand process per run. Its IPC listener binds loopback for that worker and exits
after the run is terminal and idle.

## MCP configuration

The installer uses an absolute executable path:

```console
codex mcp add session-skein -- /absolute/path/to/skein mcp
codex mcp add session-skein -- /absolute/path/to/skein mcp --allow-control
```

Inspect it with `codex mcp get session-skein --json`. The repository plugin instead
uses `skein` through `PATH` because plugin manifests cannot carry one portable
OS-specific absolute path.

## Related operations

- [Back up and restore](maintenance.md#backup-and-restore)
- [Reset or uninstall](maintenance.md#reset-and-data-purge)
- [Privacy limits](privacy.md)
- [Context source policy](context-recall.md)
