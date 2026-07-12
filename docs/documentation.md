# Documentation maintenance

The handbook is a product interface. Keep it navigable, current, bounded, and safe to
publish.

## Structure

- `README.md` explains the outcome and shortest path.
- `INSTALL.md` is the normative human/agent installation contract.
- `docs/index.md` is the handbook navigation root.
- Task guides explain one workflow.
- Reference pages enumerate stable commands, tools, state, and schemas.
- `codebase-map.md` teaches relationships and guided source paths.
- `architecture.md` and `privacy.md` explain design constraints.

Do not duplicate an entire reference into a skill. The skill should contain the
minimal decision workflow and link to focused references.

## Change checklist

When adding or changing a command, MCP tool, environment variable, state file,
installer option, key binding, or trust boundary:

1. update the relevant task guide;
2. update the CLI/MCP/state reference;
3. update the codebase map if module ownership or a data flow changed;
4. update the bundled skill only if agent procedure changed;
5. add or update a drift test; and
6. run the complete checks from `AGENTS.md`.

Avoid release-number narration in durable guides. Put historical detail in
`CHANGELOG.md`; describe current behavior in the handbook.

## Public-safety rules

- Use synthetic paths such as `/path/to/project`.
- Never paste real transcript text, credentials, private URLs, usernames, hostnames,
  or capability values.
- Document privacy defaults and control authority beside the command that changes
  them.
- Keep external links authoritative and use repository-relative links internally.

## Teaching-map policy

The project borrows the useful output shape of codebase-understanding tools: project
purpose, components, typed relationships, layers, flows, and a guided tour. Curated
Markdown is the canonical public artifact today. A future deterministic structural
map may feed the TUI and MCP, but generated prose must never become required for
routing, startup, or control.
