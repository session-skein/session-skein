# Privacy and control boundaries

## Default local data

Session Skein stores canonical project paths, bounded Git metadata/identity text,
content-free Codex session metadata, and redaction-safe run/audit state in a private
per-user SQLite database. It does not store control prompts or model transcripts.

## Optional sensitive sources

- Generated Codex memory Markdown is a separate opt-in.
- Raw Codex user/assistant session messages are another opt-in and require a canonical
  existing cwd beneath a canonical approved scan root.
- Admitted text is bounded but not comprehensively secret-redacted.
- MCP deep-context results enter the model context and may reach the configured model
  provider.

Never expose snippets merely to improve apparent routing confidence.

## Control

The control-enabled server exposes conduct, steer, interrupt, and reconcile. Presence
of a tool or its MCP annotation is not authority.

- Conduct requires a unique high-confidence route, explicit full-access
  acknowledgement, and UUID.
- Steer targets the exact active fenced run and requires UUID.
- Interrupt targets the exact active fenced run.
- Reconcile targets an exact recovery-required run and may durably record source
  evidence; it never replays or takes over work.

Observation requests do not authorize control. A user's standing preference for
full-access can satisfy the policy acknowledgement only when the current request
actually asks to execute or change work.

## Output discipline

Prefer project identity, evidence categories, opaque IDs, counts, and redacted state.
Do not echo raw prompts, transcript snippets, credentials, capability values,
commands, diffs, or model output unless the user explicitly requests that content and
the selected operation supports it.
