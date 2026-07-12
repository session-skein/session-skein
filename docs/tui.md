# Standalone TUI

`skein tui` is a keyboard-first view over Session Skein's existing local registry,
conductor, and reconnectable Codex workers. Codex CLI and its existing ChatGPT login
are the runtime. The TUI does not require Agent Deck, tmux, MCP, systemd, a daemon, an
API key, or a second model subscription.

## Layout

- **Projects** shows deterministic cards for explicitly registered roots.
- **Work** shows Codex session identities and Skein-owned runs for the selected
  project. Selecting a run follows its bounded, redacted in-memory events.
- **Activity** shows the selected project narrative and today's metadata digest when
  no live events are available. It pins an actionable blocker above the stream when
  durable run or session state requires recovery or prevents a safe resume.
- **Global conductor composer** sends one private prompt through the same fail-closed
  `conduct` command used by scripts.

The interface uses Ratatui and Crossterm directly. Its visual roadmap follows the
same ideas as Charm without requiring Go: component-local state, adaptive themes,
Markdown-rich detail views, reusable inputs/lists/spinners, and restrained transitions.
Candidate Rust-native additions include `tui-markdown` or `ratatui-markdown` for
Glamour-like rendering and `tachyonfx` for effects; usability and low input latency
take priority over animation.

On startup, a separate background thread discovers configured roots and refreshes
bounded Git/project-document sources; this may take time on a network disk but never
blocks keyboard input or rendering. Its completion status appears in the footer.
Ordinary two-second catalog refreshes remain read-only SQLite access. Worker
snapshots, interrupts, and conductor children also run away from the rendering loop,
so a slow project disk does not directly block keyboard input.

## Keys

| Key | Action |
| --- | --- |
| `Tab` / `Shift-Tab` | Move focus |
| `Up` / `Down` | Select a project, session, or run |
| `F2` | Arm full access for exactly the next dispatch |
| `Enter` | Dispatch when the composer is focused and armed |
| `x`, then `x` | Revalidate and interrupt the exact selected active run |
| `Esc` | Disarm or cancel a pending confirmation |
| `r` | Refresh the read-only catalog |
| `q` | Quit outside the composer |
| `Ctrl-C` | Force-quit, including during conductor handoff |

Printable `q`, `r`, and `x` are ordinary text while the composer has focus. Pasted or
typed prompts are bounded to 64 KiB. Oversized pastes are rejected as a whole.

## Dispatch and recovery contract

F2 is a visible, one-shot acknowledgement of the existing danger-full-access and
never-approve policy. The TUI clears the composer and consumes that acknowledgement
before starting a child. It assigns a request UUID and shows it immediately. The
child receives prompt content only on stdin and returns one bounded JSON response.

If the response is malformed, oversized, or lost after durable planning, the TUI
reconciles status by request UUID without replaying the prompt. Prompt bytes are not
written to Session Skein's database, logs, argv, or environment. As with any normal
process memory, this is a non-persistence guarantee rather than secure memory erasure.

Normal `q` waits until an in-flight conductor handoff returns, then quitting does not
interrupt the dispatched worker. `Ctrl-C` force-quits and may race a handoff that has
not yet produced a durable receipt; the TUI never retries it. On restart, durable
projects, sessions, runs, receipts, and terminal states are loaded again; only the
bounded live event window may be gone. Use the exact-run interrupt confirmation when
termination is intended.

Session Skein does not currently enforce a single TUI owner. Concurrent TUI or CLI
clients are safe at the registry and fenced-worker boundaries, but their selections
and composers are independent.
