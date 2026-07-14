---
name: session-skein
description: Install, configure, index, search, recall, route, conduct, monitor, steer, interrupt, and recover local Codex work with Session Skein. Use when a user mentions Session Skein or skein, asks which project or session contains prior work, wants one conductor entry point across repositories, wants existing Codex sessions indexed, or asks to operate a Session Skein worker through MCP or CLI.
---

# Session Skein

Use Session Skein as the local source of truth for project/session identity and
Skein-owned runs. Keep state transitions in its CLI or MCP server; do not reproduce
its database, matching, or worker logic in the skill.

## Choose the surface

1. Prefer Session Skein MCP tools when available.
2. Fall back to the installed `skein` CLI when MCP is missing or when diagnosing its
   configuration.
3. Do not copy MCP-only fields into CLI commands. Check `skein COMMAND --help` when a
   fallback option is not listed in the reference; for example, MCP `list_sessions`
   accepts `limit`, while CLI `session list` currently does not.
4. Read `references/mcp-workflows.md` for exact MCP sequences.
5. Read `references/cli-fallback.md` for shell equivalents and installation checks.
6. Read `references/privacy-and-control.md` before enabling private context or
   starting/changing Codex work.

## Orient before acting

1. Call `get_activity_status` or run `skein doctor` and `skein project list`.
2. If setup is required, use only a project/root the user named or clearly placed in
   scope. Never infer permission to scan a home directory, drive, or parent workspace.
3. Register an exact repository when one is enough. Add a recursive scan root only
   after the user explicitly requests recursion; keep the depth bounded.
4. Refresh the index, then search. Do not guess a project from recency alone.
   Prefer `refresh_index {"project":"/exact/repository"}` when one registered
   project is enough, or `refresh_index {"scan_root":"/approved/root"}` when only
   one configured root should be traversed. Never pass both selectors. Scoped calls
   intentionally defer the global context and session sources.

## Recover before starting duplicate work

1. For prior-session content questions, call `search_sessions` using distinctive
   terms. It returns exact thread IDs and `codex resume` commands only from explicitly
   enabled raw-session projections or exact one-rollout memory summaries. Use
   `session search` as CLI fallback; add `--refresh` only when the user asked to
   refresh private recall.
2. Search projects when the question is about repository identity rather than raw
   session content.
3. Inspect the selected project, linked sessions, and active/recovery runs.
4. Prefer an exact existing session or run when it represents the requested work.
5. Surface ambiguity and evidence. Ask for direction instead of choosing a weak
   route.
6. Reconcile a recovery-required run before starting replacement work. Never replay a
   prompt whose dispatch outcome is uncertain.

## Preserve context consent

Use identity documents and content-free metadata by default. Set
`include_deep_context=true` only when the user wants enabled generated-memory or raw
session snippets used for this query. Those snippets enter Codex model context.

Do not enable generated-memory or raw-session indexing merely because a search has
few results. Explain each source and obtain intent first. Raw-session recall also
requires canonical existing session directories beneath approved scan roots.

## Preserve control authority

Treat a request to inspect, search, summarize, or suggest as read-only. Start or
change Codex work only when the user asks for execution.

For `conduct`:

- require the control-enabled MCP profile;
- create a fresh caller-owned UUID;
- pass the private prompt once;
- set `full_access_acknowledged=true` only when the user authorized that policy; and
- reuse the same UUID only to recover a lost response.

Target steer, interrupt, read, and reconcile operations by exact Skein run ID. Inspect
current state first. Never synthesize a thread/turn ID or bypass fencing.

## Report results

State the selected project and evidence, whether an existing session/run was reused,
the run/request ID for control work, and any deferred/unavailable sources. Do not
echo private search terms, transcript snippets, prompts, capabilities, or credentials
unless the user explicitly requested content and the active surface permits it.
