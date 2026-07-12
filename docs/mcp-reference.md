# MCP tool reference

`skein mcp` exposes local Session Skein state over stdio. `skein mcp
--allow-control` adds the four tools in the control section. All input schemas reject
unknown properties.

The server returns structured JSON inside MCP content. A fresh database may return a
`setupRequired` response with exact onboarding actions rather than a generic failure.

## Tool summary

| Tool | Class | Required input | Important defaults |
| --- | --- | --- | --- |
| `search_projects` | read | `query` | limit 10; session/deep text false |
| `get_project` | read | `project_id_or_path` | exact ID, path, or name |
| `suggest_codex_command` | read | `query` | deep context false |
| `list_projects` | read | none | all registered projects |
| `list_scan_roots` | read | none | all approved roots |
| `list_sessions` | read | none | limit 100; optional project |
| `list_runs` | read | none | limit 100; optional project/active |
| `get_run` | read | `run_id` | redaction-safe detail |
| `get_day_summary` | read | none | local today; optional ISO date |
| `get_recent_activity` | read | none | 24 hours; limit 50 |
| `get_activity_status` | read | none | catalog counts/timestamps |
| `get_context_settings` | read | none | private source status |
| `set_codex_memory_indexing` | write | `enabled` | applied by later refresh |
| `set_codex_session_indexing` | write | `enabled` | applied by later refresh |
| `add_project` | write | `path` | optional name |
| `add_scan_root` | write | `path` | recursive false; depth 16 |
| `remove_scan_root` | write/destructive | `path` | projects retained |
| `refresh_index` | write | none | optional mutually exclusive project/scan_root |
| `refresh_activity` | open-world write | none | 7 days; no Git; 100 sessions |
| `sync_codex_sessions` | open-world write | none | all pages; 100/page |
| `conduct` | control | prompt, authority, UUID | session text false |
| `steer_run` | control | run ID, prompt, UUID | exact active turn |
| `interrupt_run` | control | run ID | exact active turn |
| `reconcile_run` | control/write | run ID, UUID | no replay/takeover |

## Search and project tools

### `search_projects`

```json
{
  "query": "renderer investigation",
  "limit": 10,
  "include_session_text": false,
  "include_deep_context": false
}
```

`query` is 1–65,536 characters and `limit` is 1–50. Deep context is false even when
sources are enabled. Set it true only when the user intends private memory/session
snippets to enter model context. Results contain ranked evidence and bounded document
hits, never whole source documents. The `recall` diagnostic reports quick/deep mode,
consulted sources, private gates, conservative freshness, limits, possible truncation,
and an explicit escalation input. Quick mode does not query private context rows.
Deep results and `sourcesConsulted` are filtered by the live memory/session gates, so
disabling either source revokes its retained rows immediately without a refresh.

### `get_project`

Accepts `project_id_or_path` as a numeric ID string, exact registered path, or exact
project name. Returns a deterministic project card with latest observed metadata.

### `suggest_codex_command`

Accepts `query` and optional `include_deep_context`. Returns a `codex -C` argv/shell
suggestion only when the match is unambiguous; otherwise it returns candidates and
evidence without guessing.

### `list_projects` and `list_scan_roots`

Take `{}`. Project cards are generated on read. Scan roots include exact/recursive
policy and maximum depth.

## Session and activity tools

### `list_sessions`

Optional `project` selects an exact registered project identity; `limit` is 1–500 and
defaults to 100. Output is content-free session metadata.

### `list_runs`

Optional `project`, `active_only` (default false), and `limit` (1–500, default 100).
Output contains redaction-safe run and worker state.

### `get_run`

Requires positive integer `run_id`. Returns policy, source turn identity, actions,
bounded events, and worker record without prompt or model transcript.

### `get_day_summary`

Optional `date` must be `YYYY-MM-DD`; omission means today in local time. The result
is deterministic metadata prose plus coverage.

### `get_recent_activity`

`hours` is 0–87,600 and defaults to 24. `limit` is 1–500 and defaults to 50. Optional
`project` filters exact identity. This compatibility view never reads transcripts.

### `get_activity_status`

Returns catalog counts, latest durable activity timestamps, and structured
`catalogFreshness` using the fixed 24-hour default boundary. Source staleness uses the
oldest covered observation; newest timestamps remain available for diagnostics. MCP
refreshes never emit CLI progress because protocol stdout remains JSON-RPC only.

Takes `{}` and returns counts and latest redaction-safe activity timestamps.

## Context policy tools

### `get_context_settings`

Takes `{}` and reports independent defaults-off settings, source accounting, and the
private local storage scope.

### `set_codex_memory_indexing`

Requires boolean `enabled`. It changes generated memory-summary policy; run
`refresh_index` to apply it to documents.

### `set_codex_session_indexing`

Requires boolean `enabled`. It changes raw user/assistant message policy. Approved
scan-root and canonical existing cwd checks remain mandatory. Run `refresh_index` to
apply it.

## Catalog and refresh tools

### `add_project`

Requires existing directory `path`; optional non-empty `name`. It registers exactly
that directory and does not crawl a parent.

### `add_scan_root`

Requires existing directory `path`. `recursive` defaults false. `max_depth` is 0–64
and is meaningful only for recursion; normal default is 16. It immediately discovers
repositories under the approved policy.

### `remove_scan_root`

Requires stored `path`, which may be offline. It removes discovery authorization and
retains discovered projects.

### `refresh_index`

Optional `working_tree` and `force` default false. Optional string selectors `project`
and `scan_root` are mutually exclusive. With neither selector it performs the same
coordinated global refresh as CLI `index`. Project scope refreshes only that registered
project and performs no discovery. Root scope traverses only that configured root and
refreshes its provenance-linked projects. Unknown selectors fail before traversal.
Offline roots retain cached project relationships and return a deferral. Scoped calls
also report context and session synchronization as deferred because those are global
atomic sources.

Global refresh results include the raw-session `mode` and file/byte/record accounting.
MCP stdout remains JSON-RPC only and never includes private checkpoint content.

### `refresh_activity`

Compatibility refresh with:

- `since_days`: 0–3650, default 7;
- `include_git`: default false;
- `session_limit`: 1–1000, default 100;
- `max_pages`: 1–100, default 100; and
- `max_threads`: 1–10,000, default 10,000.

It synchronizes content-free sessions and optional Git metadata; it never reads raw
transcripts.

### `sync_codex_sessions`

`limit` is 1–1000 (default 100), `all_pages` defaults true, `max_pages` is 1–100
(default 100), and `max_threads` is 1–10,000 (default 10,000). It uses the installed
Codex app-server and existing authentication.

## Control tools

These tools do not exist unless the server starts with `--allow-control`.

### `conduct`

```json
{
  "prompt": "run the focused tests in the renderer project",
  "full_access_acknowledged": true,
  "request_id": "36c4c8e6-f1c4-4ef2-9390-1a3915630067",
  "include_session_text": false
}
```

Prompt length is 1–65,536. Authority must be literal `true`; `request_id` must be a
UUID. A repeated UUID returns status and never resubmits prompt content. Dispatch
requires a unique high-confidence route.
Optional `project_id` and `source_thread_id` select identities from the prior ranked
report; a thread requires its project. Explicit selectors preserve the original
prompt, are transactionally revalidated, and cannot select an unranked identity.

### `steer_run`

Requires positive `run_id`, 1–65,536 character `prompt`, and caller UUID. It queues
text only onto the exact active turn owned by the fenced worker. A retry with the same
UUID is idempotent.

### `interrupt_run`

Requires positive `run_id` and interrupts the exact active source turn.

### `reconcile_run`

Requires positive `run_id` and UUID. Source observation is read-only, but the tool
can record reconciliation evidence and change the durable run to terminal or
recovery-required state. It never replays or takes over work.

## Tool annotations

Read tools are annotated read-only. Mutating catalog/index tools are not. Conduct,
steer, interrupt, and scan-root removal are marked destructive where appropriate;
Codex session calls and control are marked open-world. Most writes with stable inputs
are annotated idempotent. These annotations are client hints and never replace
Session Skein's policy checks.

## Recommended agent sequence

```text
get_activity_status
    -> setupRequired? add_project/add_scan_root + refresh_index
    -> search_projects
    -> unique? get_project + list_sessions/list_runs
    -> existing active/recovery run? inspect or recover it
    -> explicit user request to execute? conduct with UUID + authority
    -> monitor with get_run/list_runs; steer/interrupt only by exact run ID
```

See [MCP setup](mcp.md) for registration profiles and the bundled
[`session-skein` skill](../plugins/session-skein/skills/session-skein/SKILL.md) for
Codex-specific workflow rules.
