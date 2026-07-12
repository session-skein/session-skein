# Codex MCP server

Session Skein exposes its local project/session catalog and audited Codex controls as
an on-demand MCP stdio server:

```console
codex mcp add session-skein -- skein mcp --allow-control
codex mcp get session-skein --json
```

Start a new Codex CLI or IDE session after registration. In the Codex TUI, `/mcp`
shows startup status and the discovered tools. Codex CLI and its existing ChatGPT
login remain the runtime; the MCP process has no socket listener, daemon, API key,
OAuth flow, systemd unit, tmux dependency, or Agent Deck dependency.
Server startup creates or migrates the owner-private database, so `search_projects`
can return structured setup guidance even before `skein init` has been run.

For user-wide registration with an absolute executable, replace `skein` above with
the result of `command -v skein`. A project may instead configure the same command in
trusted `.codex/config.toml`.

## Approval boundary

Every tool advertises MCP annotations. Catalog reads are marked read-only and closed
world. Project/session refresh and registration are writes. `conduct`, `steer_run`,
and `interrupt_run` are destructive/open-world where appropriate. The annotations
help Codex choose approval behavior but do not replace Session Skein's own checks.

For a personal Codex configuration, `default_tools_approval_mode = "writes"` prompts
for non-read-only tools while allowing catalog reads automatically:

```toml
[mcp_servers.session-skein]
command = "skein"
args = ["mcp", "--allow-control"]
default_tools_approval_mode = "writes"
startup_timeout_sec = 10
tool_timeout_sec = 120
```

Worker-control tools are absent unless the MCP process is explicitly started with
`--allow-control`. This startup capability is the durable user/admin boundary; tool
annotations and model-supplied arguments are not authorization. Register plain
`skein mcp` for recall and indexing without start, steer, interrupt, or reconcile.

`conduct` independently requires `full_access_acknowledged=true` and a caller-created
request UUID. It invokes the same atomic conductor used by the CLI. Reusing a UUID is
status-only and never replays prompt content. Prompt-bearing `steer_run` also requires
its caller UUID. `reconcile_run` requires a caller UUID as well so its durable
evidence/state update can be retried without inventing a new operation identity.

## Native tools

Read-only catalog and recovery tools:

- `search_projects`
- `get_project`
- `suggest_codex_command`
- `list_projects`
- `list_scan_roots`
- `list_sessions`
- `list_runs`
- `get_run`
- `get_day_summary`
- `get_recent_activity`
- `get_activity_status`
- `get_context_settings`

Explicit writes and control:

- `set_codex_memory_indexing`
- `set_codex_session_indexing`
- `add_project`
- `add_scan_root`
- `remove_scan_root`
- `refresh_index`
- `refresh_activity`
- `sync_codex_sessions`
- `conduct`
- `steer_run`
- `interrupt_run`
- `reconcile_run`

All MCP results are structured JSON. Session and run results are content-free or
redaction-safe. Live answer text, commands, diffs, approval payloads, and MCP payloads
remain Codex-owned.

See the [complete MCP reference](mcp-reference.md) for every argument, default,
bound, annotation, privacy gate, and retry contract.

## Codex Brain compatibility

The high-value former Codex Brain names are retained so natural project recall keeps
working:

- `search_projects`, `get_project`, and `suggest_codex_command` use Session Skein's
  deterministic evidence, registered project/session metadata, and bounded project
  identity-document matches. Enabled deep recall is deliberately not returned by a
  legacy-shaped call alone: after confirming user intent, pass
  `include_deep_context=true` to search or suggestion so private memory/session
  snippets may enter Codex's model context.
- `list_scan_roots` returns separately persisted discovery roots and their policies.
- `add_scan_root` registers an approved root. `recursive=false` examines only that
  directory; `recursive=true` discovers nested Git repositories to a bounded depth.
- `remove_scan_root` removes the discovery policy even if its disk is unmounted. It
  deliberately retains already discovered projects and their durable relationships.
- `refresh_index` runs configured discovery before refreshing bounded Git metadata,
  private identity documents, content-free Codex session metadata, and any explicitly
  enabled context sources, without fetching Git remotes.
- `refresh_activity` synchronizes content-free app-server session metadata updated
  inside its requested `since_days` window and can optionally refresh bounded Git
  metadata.
- `sync_codex_sessions` follows Codex cursors by default, bounded to at most 100 pages
  and 10,000 content-free thread records unless smaller limits are supplied.
- `get_recent_activity` covers durable controlled runs, not transcript messages.
- Raw-session and generated-memory setters persist explicit opt-ins. `refresh_index`
  applies them; raw sessions remain restricted to approved roots and bounded
  user/assistant message projections.

This restores scan-root, project-document, and defaults-off bounded context recall
while keeping recursion explicit. Remaining roadmap work includes incremental
activity ingestion, richer repository maps/descriptions, and an optional scheduler.
