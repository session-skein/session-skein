# Session Skein handbook

This handbook is the maintained map of Session Skein. It is organized as a
teaching path: begin with the job the product does, follow the main workflows, then
open reference pages only when you need exact contracts.

## Choose a path

| I want to... | Start here |
| --- | --- |
| Install Session Skein into Codex | [Installation](../INSTALL.md) |
| Add my first project or workspace | [Getting started](getting-started.md) |
| Understand project, session, run, and worker terminology | [Concepts](concepts.md) |
| Understand how the Rust code fits together | [Codebase map](codebase-map.md) |
| Index a large or network-mounted workspace | [Indexing and search](indexing-and-search.md) |
| Use one prompt to route work | [Conductor](conductor.md) |
| Operate the terminal interface | [TUI](tui.md) |
| Connect Codex through MCP | [MCP setup](mcp.md) |
| Find an exact command or MCP tool | [CLI reference](cli-reference.md) / [MCP reference](mcp-reference.md) |
| Back up, update, reset, or uninstall | [Maintenance](maintenance.md) |
| Diagnose a failure or slow scan | [Troubleshooting](troubleshooting.md) |
| Evaluate privacy and authority | [Privacy](privacy.md) |

## Learning tour

1. **Purpose:** Session Skein is a local catalog and conductor, not a replacement
   for Codex. Read [concepts](concepts.md).
2. **Sources:** projects are explicit, scan roots are approved, and Codex threads
   remain Codex-owned. Read [indexing and search](indexing-and-search.md).
3. **Decision:** local evidence ranks a project; recency cannot invent a match.
   Read [matching and summaries](matching-summaries.md).
4. **Control:** a unique route plus explicit full-access authority creates an
   audited worker. Read [conductor](conductor.md) and [workers](workers.md).
5. **Surfaces:** CLI, TUI, and MCP all call the same state and policy layers. Read
   [architecture](architecture.md) and the [codebase map](codebase-map.md).

## Task guides

- [Getting started](getting-started.md)
- [Indexing and search](indexing-and-search.md)
- [Codex preview](codex-preview.md)
- [Session synchronization](session-sync.md)
- [Context recall](context-recall.md)
- [Git refresh](git-refresh.md)
- [Foreground Codex control](codex-control.md)
- [Reconnectable workers](workers.md)
- [Conductor](conductor.md)
- [TUI](tui.md)
- [MCP setup and policy](mcp.md)

## Reference

- [CLI reference](cli-reference.md)
- [MCP tool reference](mcp-reference.md)
- [State and configuration](state-and-configuration.md)
- [Codebase map](codebase-map.md)
- [Glossary](concepts.md#glossary)
- [Roadmap](../ROADMAP.md)
- [Changelog](../CHANGELOG.md)

## Operations and trust

- [Privacy and data boundaries](privacy.md)
- [Maintenance, backup, and uninstall](maintenance.md)
- [Troubleshooting](troubleshooting.md)
- [Security policy](../SECURITY.md)

## Development

- [Architecture](architecture.md)
- [Codebase map](codebase-map.md)
- [Contributing](../CONTRIBUTING.md)
- [Agent development guide](../AGENTS.md)
- [Documentation maintenance](documentation.md)

## Documentation contract

User-facing behavior must be discoverable from this page. Command names, MCP tools,
public environment variables, local links, plugin metadata, and installer version
claims are checked by the test suite. When behavior changes, update its task guide
and reference entry in the same pull request.

The information architecture borrows a useful principle from
[Understand Anything](https://github.com/Egonex-AI/Understand-Anything): a project
map should teach relationships and guided paths, not merely enumerate files. Session
Skein keeps this handbook deterministic and reviewable; it does not require an LLM or
browser dashboard to understand the repository.
