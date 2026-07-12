---
layout: default
title: Overview
---

# One place for your Codex work

Session Skein is a fast, local-first control plane for Codex CLI projects, threads,
recall, and audited workers. It adds a project library, search, conductor prompt, and
keyboard-first TUI while keeping Codex CLI and your existing ChatGPT login first-class.

## Install the current preview

Linux and macOS:

```console
curl -fsSL https://raw.githubusercontent.com/session-skein/session-skein/main/install.sh | bash -s -- --control
```

The installer resolves the maintained preview channel, verifies the exact release
manifest and SHA-256 checksum, and installs the matching binary and bundled Codex
skill. The default profile is catalog-only; `--control` additionally exposes audited
worker controls. Windows users and anyone who wants to inspect or pin the installer
should follow the canonical [installation guide](INSTALL.md).

## Start in five minutes

```console
skein init
skein project add /path/to/repository
skein index
skein session sync codex --all-pages
skein tui
```

To route one prompt after indexing:

```console
printf '%s\n' 'continue the renderer investigation' | skein conduct --full-access --follow
```

Session Skein does not add scan roots, enable transcript recall, start workers, or
grant control acknowledgements during installation. Read the [quickstart](docs/getting-started.md)
and [privacy boundaries](docs/privacy.md) before opting into broader sources or control.

## Choose a path

| Goal | Canonical guide |
| --- | --- |
| Install or choose an MCP profile | [Installation](INSTALL.md) |
| Register and index projects | [Quickstart](docs/getting-started.md) |
| Update, back up, or uninstall | [Maintenance](docs/maintenance.md) |
| Understand projects, sessions, runs, and workers | [Core concepts](docs/concepts.md) |
| Review local data and trust boundaries | [Privacy](docs/privacy.md) |
| Find an exact command | [CLI reference](docs/cli-reference.md) |
| Connect Codex through MCP | [MCP setup](docs/mcp.md) |
| Route work from one prompt | [Conductor](docs/conductor.md) |
| Diagnose installation, indexing, or control | [Troubleshooting](docs/troubleshooting.md) |
| Verify preview artifacts | [Releases](docs/releases.md) and [security](SECURITY.md) |
| Contribute | [Contributing](CONTRIBUTING.md) |

## Source of truth

The repository Markdown is authoritative. This site stages and renders those files
without maintaining parallel copies. Each rendered page links to its canonical source;
behavior changes belong in the task guide and reference page in the same pull request.
