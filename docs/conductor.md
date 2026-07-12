# One-prompt conductor

After dispatch, use `worker observe` (or MCP `observe_run`) with its stable cursor for
durable monitoring. Interrupt acceptance is a request phase, not terminal completion.

`conduct` is the first standalone entry point above matching and reconnectable Codex
workers:

```console
printf '%s\n' 'continue Session Skein routing work' | \
  skein conduct --full-access
```

It requires only Session Skein, the locally installed Codex CLI, and that CLI's
ChatGPT login. Agent Deck, tmux, MCP, systemd, an API key, and a background daemon are
not involved.

## Fail-closed route

The prompt is limited to 64 KiB and read exactly once. Skein first performs a
read-only metadata match. No match, low/medium confidence, or ambiguity returns the
bounded evidence without launching Codex or creating control state. A unique
high-confidence result receives a content-free ChatGPT-account preflight.

Refusals include ranked candidates with stable project IDs, optional opaque session
IDs, content-free evidence, and exact selector fields. Preserve the original prompt
and resolve only the selected ranked identity:

```console
printf '%s\n' 'the exact original prompt' | \
  skein conduct --full-access --project-id PROJECT_ID
printf '%s\n' 'the exact original prompt' | \
  skein conduct --full-access --project-id PROJECT_ID --session-id SOURCE_THREAD_ID
```

Selectors are revalidated inside the planning transaction. They cannot select an
unranked project/session or a worker, and accepted decisions record
`resolutionKind: user_selected`.

After authentication, Skein begins an immediate SQLite transaction and recomputes the
route. The selected project, start/resume action, and optional exact thread must match
the preflight result. The transaction atomically records:

- the explicit danger-full-access/no-approval policy snapshot;
- the control run, turn, actions, and initial action events;
- a content-free route receipt and its structured evidence; and
- the fenced starting worker claim.

Only then is the detached worker process spawned and given the unchanged in-memory
prompt. The worker verifies ChatGPT authentication and effective full-access policy
again before its first Codex mutation.

Automatic resume requires an exact, non-ephemeral, project-bound Codex thread that is
not a subagent/system-error observation and has no active or recovery-required Skein
owner. Completed Skein-owned threads are eligible immediately from their audited run
identity, even before a later session-catalog sync. A high-confidence project match
without such an exact thread starts a separate thread; it never silently steers an
existing run.

## Retry and output contracts

```console
printf '%s\n' 'continue the renderer' | \
  skein conduct --full-access --request-id UUID --json

printf '%s\n' 'continue the renderer' | \
  skein conduct --full-access --request-id UUID --jsonl
```

`--json` returns one launch/status object and detaches. `--jsonl` implies follow and
emits route, worker snapshot, redacted events, and terminal run records. `--follow`
does the same in human-readable form; detaching does not interrupt Codex.

A repeated request UUID is only a durable status lookup. Because prompt content and
hashes are deliberately not stored, Skein ignores retry stdin, creates no second run,
and never resends a possibly lost prompt. A new attempt requires a new UUID.

If the process dies after atomic planning but before any Codex mutation, the starting
lease expires into a fixed pre-dispatch failure. If a mutation entered dispatching,
the existing uncertainty/reconciliation rules apply. Neither path replays work.
