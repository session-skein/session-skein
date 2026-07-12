# Explicit Codex control

Session Skein can run one foreground Codex turn against an explicitly registered
project. It talks directly to the installed Codex app-server and uses its existing
ChatGPT authentication. It never reads or copies Codex credentials and does not
require an API key.

```console
printf '%s\n' 'Run the focused tests.' | \
  skein control codex /path/to/registered/project --full-access --jsonl
```

Resume a specifically selected thread:

```console
printf '%s\n' 'Continue the previous task.' | \
  skein control codex /path/to/registered/project \
    --resume THREAD_ID --full-access --jsonl
```

The resume thread must first exist in Skein's durable catalog and be bound to the
selected project. Synchronize metadata, inspect the relationship, and explicitly bind
it when automatic path matching is not sufficient:

```console
skein session sync codex --json
skein session show THREAD_ID --json
skein session bind THREAD_ID /path/to/registered/project --json
```

This foreground compatibility command does not select the project or thread. For a
reconnectable process and exact interruption, use the separately documented
`skein worker` commands. The worker path adds exact-turn steer and read-only source
observation plus durable reconciliation. The global conductor and TUI build on the
worker path and add routing/confidence evidence; worker lease takeover or reattachment
after process loss remains unimplemented.

## Policy boundary

`--full-access` is required for every run. It records and sends the protocol equivalent
of the user's full-access CLI mode:

```text
thread start/resume: sandbox=danger-full-access, approvalPolicy=never
turn start:          sandboxPolicy.type=dangerFullAccess, approvalPolicy=never
```

Session Skein checks the effective start/resume response and fails closed if Codex or
managed policy changed either setting or the working directory. The immutable policy
snapshot also records network access, project, acknowledgement source, and time.

## Audit and crash behavior

Policy, run, turn, both initial actions, and their first events commit atomically before
the first mutating request. Each action transitions conditionally from planned to
dispatching and then to its acknowledged or terminal state. Terminal run truth comes
only from the exact matching `turn/completed` notification.

If transport is lost after dispatch, Session Skein marks the action uncertain and the
run `recovery_required`. After independently verifying that the foreground controller
is dead, `skein control mark-stale --force` quarantines its in-flight records without
replaying them. The force flag applies to legacy foreground runs, which have no worker
lease and could otherwise quarantine a still-live controller. Lost worker runs have a
separate audited `worker reconcile` command that can apply exact terminal Codex source
truth. Durable reattachment is still planned; neither command fabricates completion or
silently creates replacement work.

## Content boundary

Prompts arrive through stdin and are forwarded to Codex. Session Skein stores only the
input byte count and an opaque client correlation ID. It does not store prompt text or
hashes. Agent messages and tool activity are likewise not persisted.

JSONL monitoring redacts agent-message deltas by default and reports only their byte
length. `--include-content` displays source-owned live deltas for that invocation only.
Run and action inspection is always content-free:

```console
skein control list --json
skein control show RUN_ID --json
```

An unexpected server-initiated approval, MCP elicitation, or user-input request fails
closed instead of being silently approved.

The foreground controller does not manage or reconcile background terminals or
detached subprocesses created by Codex. Such processes may outlive the controlled turn
because the stable app-server does not expose background-terminal enumeration or
cleanup.
