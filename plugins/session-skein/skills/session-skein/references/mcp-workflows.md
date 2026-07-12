# MCP workflows

## Fresh setup

1. `get_activity_status {}`.
2. If `setupRequired`, choose exactly one:
   - `add_project {"path":"/explicit/repository"}`; or
   - `add_scan_root {"path":"/explicit/workspace","recursive":true,"max_depth":16}`
     only with recursive authorization.
3. Refresh the narrowest useful scope: `refresh_index {"project":"/explicit/repository"}`
   or `refresh_index {"scan_root":"/explicit/workspace"}`. Use `refresh_index {}`
   only when global context/session synchronization and every configured root are
   intended. The selectors are mutually exclusive.
4. `sync_codex_sessions {}` when a full bounded session pass is useful.

## Find prior work

1. `search_projects {"query":"distinctive terms"}`.
2. If unique, `get_project {"project_id_or_path":"..."}`.
3. `list_sessions {"project":"..."}`.
4. `list_runs {"project":"...","active_only":false}`.
5. Use `get_run` on active or recovery candidates before proposing new work.

Never let recency alone choose a project. Return candidates/evidence when matching is
ambiguous.
Resolve ambiguity only with the chosen ranked `project_id` and optional
`source_thread_id` in `conduct`; do not rewrite the prompt or guess an identity.

## Deep recall

1. `get_context_settings {}`.
2. Explain enabled sources and model-context disclosure.
3. With user intent, repeat `search_projects` or `suggest_codex_command` with
   `include_deep_context:true`.

Use `recall` diagnostics to explain sources, freshness, limits, and possible
truncation. Enabling and refreshing a source are separate consented operations.

To change policy, call the appropriate `set_codex_*_indexing` tool, then
`refresh_index`. Enabling generated memories never authorizes raw sessions.

## Conduct

1. Complete the prior-work workflow.
2. Resolve active/recovery state first.
3. Generate a UUID.
4. Call `conduct` with prompt, `full_access_acknowledged:true`, UUID, and normally
   `include_session_text:false`.
5. Save the returned run/request IDs in the response to the user.
6. Poll with `get_run` or `list_runs`.

A repeated UUID is a status lookup. Do not generate a new UUID to retry an uncertain
dispatch.

## Operate an existing run

- Inspect: `get_run {"run_id":N}`.
- Monitor: `observe_run` with returned `nextCursor`; timeout is nonterminal.
  Observation is read-only; call explicit reconciliation only when its diagnostics
  recommend it and the user intends that mutation.
- Steer: generate a UUID, then `steer_run` with exact run ID and text.
- Interrupt: inspect, confirm user intent, then `interrupt_run` with exact run ID.
  Treat queued as request-only and observe for acknowledgement and terminality.
- Recover: generate/reuse a reconciliation UUID, then `reconcile_run` on an exact
  recovery-required run.

Reconciliation can record durable evidence. It does not replay, resume, or take over
the source turn.
