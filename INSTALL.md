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
- `curl` and `tar` on Linux/macOS, or PowerShell 7 or newer on Windows.
- `git`, Rust 1.95 or newer, and a native toolchain only for explicit source builds.
  The repository pins Rust 1.95.0.
- A native C compiler and linker because the locked build compiles bundled SQLite:
  - Debian, Ubuntu, and WSL: `sudo apt install build-essential`;
  - Fedora: `sudo dnf group install development-tools`;
  - Arch Linux: `sudo pacman -S base-devel`;
  - macOS: run `xcode-select --install` for Xcode Command Line Tools;
  - Windows: install Visual Studio Build Tools 2022 with **Desktop development
    with C++** and a Windows 10 or 11 SDK, then use a Developer PowerShell.

The normal installer selects a published native archive, verifies it against both
`release-manifest.json` and `SHA256SUMS`, safely extracts it, validates the executable
version, and installs the archive's matching bundled skill. `--binary` / `-Binary`
remains available for an already-built native executable.

Check the two required programs:

```console
codex --version
codex login status
```

## Linux and macOS

Inspect the bootstrap script, then install the approved preview channel:

```console
curl -fsSL https://raw.githubusercontent.com/session-skein/session-skein/main/install.sh | \
  bash -s -- --control
```

The script is fetched once. It resolves `release-channels/preview`, then pins every
metadata and archive request to that exact `vVERSION` GitHub Release; it never
re-fetches an installer after resolution. To pin the release yourself:

```console
curl -fsSL https://raw.githubusercontent.com/session-skein/session-skein/main/install.sh | \
  bash -s -- --version 0.5.0-alpha.10 --control
```

Useful options:

```text
--catalog-only        register read/catalog MCP tools only (default)
--control             also expose conduct/steer/interrupt/reconcile tools
--binary PATH         install an already-built skein binary
--source PATH         explicitly build and install this checkout
--version VERSION     install one exact published preview
--channel preview     install the approved preview channel (default)
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

`--update` remains a source-checkout operation: it fast-forwards the checkout and
re-runs its refreshed installer. The
active skill never points into that mutable checkout. A build, validation, or
initialization failure leaves the prior content-addressed skill snapshot active.
Binary-first updates stage and validate the incoming installer snapshot before any
owned destination changes and retain the prior snapshot and receipt until the full
installation transaction succeeds.

`--no-skill` and `--no-mcp` mean “do not change this integration.” On a repeated
installation they preserve previously owned receipt entries rather than stranding
them. `--binary PATH --no-skill` needs no source checkout; the supplied executable
must identify itself as Session Skein and return a valid JSON `doctor` report.

## Windows PowerShell

```powershell
$installer = Join-Path $env:TEMP 'session-skein-install.ps1'
Invoke-WebRequest https://raw.githubusercontent.com/session-skein/session-skein/main/install.ps1 -OutFile $installer
& $installer -Control
```

Downloading to a file keeps parameters and errors explicit. `-Version
0.5.0-alpha.10` pins a release; otherwise `-Channel preview` resolves the same approved
preview pointer as Unix.

PowerShell parameters mirror the Unix installer: `-Control`, `-CatalogOnly`,
`-Binary`, `-Source`, `-Version`, `-Channel`, `-BinDir`, `-ReplaceBinary`, `-NoMcp`,
`-NoSkill`, `-ReplaceMcp`, `-ReplaceSkill`, `-Update`, and `-Uninstall`.

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

## Contributor/source installation

Source builds are explicit:

```console
git clone https://github.com/session-skein/session-skein.git
cd session-skein
./install.sh --source . --control
```

Windows uses `./install.ps1 -Source . -Control` from a Developer PowerShell.

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

Alpha.8 users must rerun the binary-first installer once because that published
binary cannot contain the new command. From alpha.9 onward:

```console
skein update --check
skein update
```

Product update accepts only unchanged release-owned receipts. Source contributors
continue using the explicit source installer flow below.

For an explicit source checkout:

```console
./install.sh --update --control
./install.sh --uninstall
```

Windows source mode uses `./install.ps1 -Update -Control` and
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
