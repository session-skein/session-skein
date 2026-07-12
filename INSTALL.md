# Install Session Skein

This is the normative installation contract for humans and coding agents. A Codex
instance asked to install this repository should follow this page, not infer setup
from the Rust workspace.

## What installation changes

A normal installation does exactly four things:

1. installs the `skein` executable in a user-writable binary directory;
2. initializes or migrates Session Skein's private per-user database;
3. copies an immutable, content-addressed skill snapshot and links it into the active
   Codex home; and
4. registers one local stdio MCP server with an absolute executable path.

It does **not** add scan roots, recursively inspect a workspace, enable generated
memory or raw transcript recall, start a Codex worker, install a daemon, or grant a
control operation its per-request acknowledgement.

## Prerequisites

- A local Codex CLI installation with a working ChatGPT login.
- Linux, macOS, Windows, or WSL.
- `git` for the reviewable managed checkout and bundled skill.
- Rust 1.95 or newer for normal source installation. The repository pins 1.95.0.
- A native C compiler and linker because the locked build compiles bundled SQLite:
  - Debian, Ubuntu, and WSL: `sudo apt install build-essential`;
  - Fedora: `sudo dnf group install development-tools`;
  - Arch Linux: `sudo pacman -S base-devel`;
  - macOS: run `xcode-select --install` for Xcode Command Line Tools;
  - Windows: install Visual Studio Build Tools 2022 with **Desktop development
    with C++** and a Windows 10 or 11 SDK, then use a Developer PowerShell.

`--binary` / `-Binary` is the explicit advanced path for an already-built native
executable and does not require the Rust or C build toolchains.

Check the two required programs:

```console
codex --version
codex login status
```

## Linux and macOS

The reviewable checkout flow is recommended:

```console
git clone https://github.com/session-skein/session-skein.git
cd session-skein
./install.sh                 # catalog-only MCP
./install.sh --control       # catalog + audited worker control
```

The installer builds the checkout with its locked dependencies, validates the exact
`skein VERSION` and JSON `doctor` response, then copies the skill from that same
revision into an immutable snapshot. The live Codex skill link switches only after
the executable initializes successfully. Future signed native releases are tracked
separately on the roadmap.

For a one-line bootstrap, inspect the script first, then run:

```console
curl -fsSL https://raw.githubusercontent.com/session-skein/session-skein/main/install.sh | \
  bash -s -- --control
```

Useful options:

```text
--catalog-only        register read/catalog MCP tools only (default)
--control             also expose conduct/steer/interrupt/reconcile tools
--binary PATH         install an already-built skein binary
--source PATH         build and install this checkout
--bin-dir PATH        override the executable destination directory
--replace-binary      back up and replace an unowned destination binary
--no-mcp              do not create Codex MCP configuration
--no-skill            do not link the Codex skill
--replace-mcp         replace a conflicting session-skein MCP registration
--replace-skill       replace a conflicting skill path
--update              refresh the managed checkout and reinstall
--uninstall           remove installer-owned binary, skill link, and MCP entry
--help                show the complete option list
```

The installer refuses an existing binary, skill path, or MCP registration unless a
prior receipt proves ownership. The corresponding `--replace-*` option is explicit:
binary and skill replacements are moved to timestamped backups and restored on
uninstall; replaced MCP JSON is retained for manual recovery. The receipt stores the
installed binary hash, skill target, and complete MCP-configuration hash. If any of
those are later changed by the user or another package manager, uninstall preserves
the changed object and keeps the receipt for review.

`--update` fast-forwards the checkout and re-runs its refreshed installer, but the
active skill never points into that mutable checkout. A build, validation, or
initialization failure leaves the prior content-addressed skill snapshot active.

`--no-skill` and `--no-mcp` mean “do not change this integration.” On a repeated
installation they preserve previously owned receipt entries rather than stranding
them. `--binary PATH --no-skill` needs no source checkout; the supplied executable
must identify itself as Session Skein and return a valid JSON `doctor` report.

## Windows PowerShell

```powershell
git clone https://github.com/session-skein/session-skein.git
Set-Location session-skein
./install.ps1                 # catalog-only MCP
./install.ps1 -Control        # catalog + audited worker control
```

One-line bootstrap after reviewing `install.ps1`:

```powershell
iwr -useb https://raw.githubusercontent.com/session-skein/session-skein/main/install.ps1 | iex
```

PowerShell parameters mirror the Unix installer: `-Control`, `-CatalogOnly`,
`-Binary`, `-Source`, `-BinDir`, `-ReplaceBinary`, `-NoMcp`, `-NoSkill`, `-ReplaceMcp`,
`-ReplaceSkill`, `-Update`, and `-Uninstall`.

On Windows, the installer adds its binary directory to the user `PATH` only when
needed, remembers whether it did so across updates, and removes only that owned entry
on uninstall.

## Verify

Open a new shell so a newly added binary directory is visible, then run:

```console
skein --version
skein doctor
skein context status
codex mcp get session-skein --json
```

The MCP `command` must be an absolute path to the installed binary. Start a **new**
Codex session after installation; skills and MCP configuration are discovered at
session startup. In the TUI, `/mcp` should show `session-skein`.

The control profile exposes four additional tools, but every conductor dispatch
still requires `full_access_acknowledged=true` and a caller-owned request UUID.

## First index

Choose the narrowest useful authorization:

```console
# One exact repository
skein project add /path/to/repository

# Or one workspace root; recursion is explicit
skein scan-root add /path/to/workspace --recursive --max-depth 16

skein index
skein session sync codex --all-pages
skein tui
```

Generated Codex memory summaries and raw session messages remain disabled. Read
[context recall](docs/context-recall.md) before enabling either source.

## Codex plugin installation

The direct installer is the reliable control-enabled local-host path. This repository
also publishes a Codex marketplace plugin containing the same skill and a
**catalog-only** MCP declaration. Install only the binary/state first so the plugin
owns the skill and MCP surfaces:

```console
./install.sh --no-skill --no-mcp
```

Windows uses `./install.ps1 -NoSkill -NoMcp`. Then install the plugin:

```console
codex plugin marketplace add session-skein/session-skein --ref main
codex plugin add session-skein@session-skein
```

The plugin MCP declaration invokes `skein` through `PATH`;
plugins cannot portably install a native Rust executable or substitute an
OS-specific absolute path. It is intended for shell-launched Codex where the install
directory is inherited in `PATH`. Use the direct installer instead for an absolute
MCP executable or control-enabled profile. Do not keep both surfaces enabled.

To update the plugin after upgrading the repository:

```console
codex plugin marketplace upgrade session-skein
codex plugin add session-skein@session-skein
```

Codex caches installed plugins. Start a new thread after reinstalling.

## Manual source installation

The installer is convenience, not magic. Its essential source path is:

```console
cargo build --workspace --release --locked
install -m 0755 target/release/skein "$HOME/.local/bin/skein"
"$HOME/.local/bin/skein" init
codex mcp add session-skein -- "$HOME/.local/bin/skein" mcp
```

For control-enabled MCP, append `--allow-control`. Copy or symlink
`plugins/session-skein/skills/session-skein` to
`${CODEX_HOME:-$HOME/.codex}/skills/session-skein`.

## Update and uninstall

From the installed checkout or a fresh clone:

```console
./install.sh --update --control
./install.sh --uninstall
```

Windows uses `./install.ps1 -Update -Control` and
`./install.ps1 -Uninstall`.

Uninstall preserves the SQLite database, project registry, and Codex-owned files.
See [maintenance](docs/maintenance.md) for a deliberate backup, reset, or data purge.

## Agent completion checklist

An installing agent is finished only when all of these are true:

- the installed executable passes `skein --version`;
- `skein doctor` opens the intended state and reports healthy schema state;
- the requested catalog-only or control-enabled MCP command is registered with an
  absolute executable path;
- the skill resolves from the active `CODEX_HOME`;
- the agent reports that a new Codex session is required; and
- no root, transcript source, service, or worker was enabled without explicit user
  direction.
