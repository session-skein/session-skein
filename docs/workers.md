# Reconnectable Codex workers

`worker observe` is the durable reconnectable monitor. Its event-ID cursor advances
without duplicates; reports include more-events state, cancellation phase, pending
actions, terminal/recovery state, heartbeat age, lease health, and a recommended next
action. Optional wait is bounded to 30 seconds and needs no daemon. Queued or
acknowledged interrupt is not terminal cancellation; repeats are idempotent.
Observation opens state read-only on every poll. It never recovers a lease, reconciles
a run, dispatches control, or changes actions/workers; stale state only changes the
reported health and recommended next action.

Session Skein can start one explicitly targeted Codex turn in an on-demand worker.
The worker owns the Codex app-server connection; the launching CLI and later watchers
are clients and may exit without terminating the turn. No systemd unit, separately
installed service, tmux, Agent Deck, MCP client, or API key is required.

```console
printf '%s\n' 'Run the focused tests.' | \
  skein worker start /path/to/registered/project --full-access --json

skein worker list --active --json
skein worker status RUN_ID --json
skein worker observe RUN_ID --after-cursor 0 --limit 50 --json
skein worker observe RUN_ID --after-cursor CURSOR --timeout-ms 30000 --json
skein worker watch RUN_ID --jsonl
printf '%s\n' 'Only inspect the failing test.' | skein worker steer RUN_ID
skein worker interrupt RUN_ID
skein worker read RUN_ID --json
skein worker reconcile RUN_ID --json
skein worker stop RUN_ID
```

Resume requires a cataloged Codex thread bound to the selected registered project:

```console
printf '%s\n' 'Continue the task.' | \
  skein worker resume THREAD_ID /path/to/project --full-access --json
```

`--full-access` is required for every start or resume. The immutable policy snapshot
records `danger-full-access`, approval policy `never`, network authority, project,
working directory, acknowledgement source, and time. Codex's effective policy and cwd
are verified before the turn starts.

## Ownership and restart boundary

Each run gets one random worker identity, a monotonically fenced lease epoch, and one
Codex child connection. The worker heartbeats every two seconds under a ten-second
lease. Every dispatched action records the worker and epoch; acknowledgements and
completion fail if that fence is no longer current.

The worker is launched in a separate process group and owns Codex through a guard
whose stdin is an owner-liveness pipe. Closing or killing the worker closes that pipe;
the guard then kills and waits for Codex. A normal client exit does not close it.

Client restart is supported: use `worker list`, `status`, or `watch` from a fresh CLI.
Worker/app-server crash is different. Once its lease expires, a client atomically
marks the worker lost, fences the old epoch, marks ambiguous actions and turns
uncertain, and moves the run to `recovery_required`. It never resends the prompt or an
uncertain mutation. `worker reconcile` opens a fresh bounded app-server connection,
reads only the recorded thread and exact turn, and records content-free evidence. An
authoritative terminal status closes the Skein run. `inProgress`, missing, mismatched,
or unsupported source state leaves it recovery-required. Reconciliation does not
take over, resume, or reattach a lost worker.

## Steering and source reads

`worker steer` reads one bounded non-empty text input from stdin and queues it through
the worker's existing app-server connection. Codex receives `turn/steer` with the
recorded active turn as an exact precondition. The client-generated request ID is
durable, so `--request-id UUID` can safely retry a lost IPC response without a second
wire request. Multiple steers are FIFO; once interrupt is planned, later steers are
rejected. A queued steer whose text is lost before dispatch is failed, never replayed.

`worker read` uses `thread/read` on a fresh connection and emits thread/turn identities,
status labels, and whether complete item metadata was available. It does not resume
the thread, display content, or persist the response. `worker reconcile` is narrower:
it is accepted only for a worker-owned `recovery_required` run whose worker is fenced
and terminal.

## Private IPC and content

The worker listens only on an ephemeral `127.0.0.1` port. Authentication requires the
run identity plus a random capability stored in:

```text
$SKEIN_DATA_DIR/workers/run-RUN_ID.capability
```

On Unix, the directory is mode `0700` and the file is `0600`. The capability is not
stored in SQLite or exposed in JSON, argv, environment variables, or logs. Requests
and responses are size bounded. Connection handlers cannot block lease heartbeats.

Prompts are transferred once through this private channel and remain in worker memory.
Agent-message deltas are converted immediately to byte counts. A worker retains at
most 512 redacted events in memory and reports sequence gaps; it never persists raw
prompt, answer, command, diff, approval, or MCP content. After the worker exits, only
durable redacted state remains.

## Current alpha limits

- One text turn per worker run.
- Full-access/no-approval policy only, with explicit acknowledgement.
- Source reads are metadata-only; there is no source-content display.
- The conductor and TUI refuse ambiguity; there is no interactive ambiguity picker or
  semantic/LLM router.
- `worker stop` refuses an active run; use `worker interrupt`, wait for authoritative
  terminal status, then stop the idle worker.
- Reconciliation can close exact terminal turns; lease takeover/reattachment is not
  implemented.
- Codex background terminal processes are not enumerated by the stable app-server API.

See [conductor](conductor.md) for single-prompt routing and [TUI](tui.md) for the
global composer that monitors these workers.
