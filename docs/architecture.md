# Architecture

Session Skein is a local control plane. Its core model is intentionally independent
from any one agent, terminal multiplexer, or subscription provider.

```text
CLI / TUI / MCP
      |
Conductor and policy
      |
Project + session + activity model <--- optional Git/tmux/Agent Deck observers
      |
Codex CLI app-server
      |
Versioned SQLite state + Codex-owned transcripts
```

## Workspace boundaries

- `skein-core` owns paths, migrations, and domain state. It does not spawn agents.
- `session-skein` produces the `skein` CLI and turns core operations into stable
  human-readable or JSON output.
- Codex CLI is the first-class runtime; the standalone path has no session-manager
  dependency.
- Optional adapters read source data incrementally and preserve provenance without
  owning core state.
- Control operations remain separate from observation and require an explicit
  capability and per-operation policy decision.
- Matching and activity views are ephemeral projections over durable metadata. They
  persist neither the private query nor generated prose and cannot dispatch work.

## Control state

The versioned schema keeps control intent separate from imported sessions. Immutable
policy snapshots record the exact authority acknowledged for a run. Skein-owned runs,
turns, actions, and append-only events are committed before app-server mutations.
Observed session status never drives the control state machine.

The compatibility control path owns one foreground stdio app-server connection. The
worker path assigns a run to a per-run process with a heartbeat lease and fencing
epoch. Every worker mutation records and verifies that owner. A detached process owns
the Codex connection behind a child guard, while clients use authenticated loopback
IPC and may restart independently. Expired workers are fenced and their runs become
`recovery_required`; mutating requests are never replayed automatically. Exact-turn
steer uses the existing fenced connection. A separate read-only `thread/read` path can
record content-free source evidence and close an exact terminal recovery run without
taking over it. See
[workers.md](workers.md) and [codex-control.md](codex-control.md).

The conductor performs an initial read-only match, validates ChatGPT authentication,
then opens one immediate SQLite transaction. It recomputes the route and atomically
inserts its content-free receipt, policy, run, actions, and starting worker lease. A
route change aborts rather than falling through to another project. Process spawn and
prompt submission happen only after commit; an expired worker with no dispatched
action becomes a deterministic failed run, never an uncertain recovery claim.

The standalone TUI is a presentation and controller layer over these same commands
and records. Its render loop performs no database, filesystem, Codex, or worker I/O.
Background catalog reads use a read-only SQLite connection; selected-run polling and
exact-run interruption use the fenced worker API. Dispatch invokes the current
`skein conduct` executable with a private stdin prompt, a stable request UUID, and a
strictly bounded one-response JSON protocol. Invalid or lost child output is
reconciled from durable content-free state and never causes prompt replay.
One startup worker performs configured scan-root, Git, bounded document, enabled
context, and content-free Codex session refresh away from the render loop, then
requests a fresh read-only catalog snapshot.

The MCP server is another presentation/controller adapter over the same boundaries.
Codex starts `skein mcp` on demand over stdio; no listener, daemon, token, or extra
authentication exists. Read tools open the registry read-only. Project registration
and Git/session refresh use the existing bounded registry operations. Conduct runs
the same fail-closed CLI path with private stdin and a stable request UUID; steer,
interrupt, and reconciliation call the same fenced worker functions. MCP does not
implement a parallel control state machine.

Tool annotations distinguish read-only metadata calls from writes and destructive or
open-world control. They are hints for the client, not authorization. Worker-control
tools exist only when the server starts with the explicit `--allow-control`
capability. `conduct` still requires an explicit boolean acknowledgement, a
caller-owned request UUID, and records the normal
immutable policy, route receipt, actions, worker claim, and audit events before Codex
mutation.

## Scan-root and Git metadata adapters

Exact project registration and scan-root authorization are separate durable concepts.
A scan root may examine only itself or explicitly recurse to a configured maximum
depth. Discovery recognizes `.git` directories and worktree `.git` files, never
follows directory symlinks, prunes known dependency/build/cache directories, and
continues after entry-level I/O errors. The root-to-project provenance table prevents
frequent activity refreshes from needing another filesystem walk.

The Git metadata adapter observes registered projects. It stores
branch, head object, latest commit timestamp and subject, and an optional tracked-file
dirty result. A fingerprint of small Git administrative files lets the default path
avoid spawning Git when repository metadata has not changed. Working-tree checks are
opt-in and exclude untracked files and submodules.

The adapter invokes read-only local Git commands. It does not fetch, contact remotes,
change the Git index or discover repositories. See [git-refresh.md](git-refresh.md) for
the observable contract.

The project-document recall adapter prefers bounded `git ls-files` selection for
tracked README, AGENTS, manifest, and top-level documentation files. A private FTS5
row is replaced atomically only when its bounded fingerprint changes. The database
may retain up to 512 KiB of identity text per registered project; public search
projections expose only a 2 KiB snippet, title, project identity, and contributing
relative paths.

## Codex adapter

`skein-codex` is isolated from the core state model. It launches the locally installed
Codex app-server over stdio, performs the documented initialize handshake, and makes
bounded `thread/list` requests. Preview reads one page; explicit synchronization may
follow opaque cursors within page and thread caps. It does not parse Codex's private
rollout JSONL format unless the caller opts into Codex's repair mode, and preview does
not persist results.

An explicit session sync converts a bounded preview page into source-neutral durable
metadata. It stores the Codex thread identity and chronology, observed cwd, last
app-server-instance status, runtime provenance, relationship identifiers, and a
conservative project link. It does not store turns or the unstable rollout path. See
[session-sync.md](session-sync.md).

The app-server schema is specific to the installed Codex version and may evolve. The
adapter decodes only the fields it exposes publicly and fails closed on protocol or
JSON-RPC errors. See [codex-preview.md](codex-preview.md).

## Performance model

The hot path must not scan repositories. Git records bounded fingerprints and skips
unchanged source inspection. Codex exposes opaque cursors only for the current bounded
scan; durable overlapping checkpoints remain planned work. SQLite queries serve
interactive views. Slow or remote project roots are polled only when configured, and
Git inspection is bounded.

Rust reduces process startup, memory overhead, and deployment friction. It cannot
remove latency caused by a network filesystem or slow physical disk, so the design
avoids that I/O rather than relying on language speed to hide it.

## Compatibility

Source adapters are subordinate compatibility modules. Product identity and core
types do not use another product's name. The project will prefer documented formats
and commands, tolerate missing tools, and surface adapter versions in diagnostics.

See the [codebase map](codebase-map.md) for module ownership, end-to-end sequence
diagrams, and guided source paths.
