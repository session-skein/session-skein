# Indexing and search

`skein index` is the normal freshness command. It coordinates several bounded
sources while keeping their permissions and failure states separate.

## Project authorization

Exact projects and scan roots are different records:

```console
skein project add /path/to/repo
skein scan-root add /path/to/workspace --recursive --max-depth 16
```

An exact project is immediately part of the catalog. A scan root authorizes future
discovery. Removing a scan root removes that authorization and provenance but does
not delete already registered projects:

```console
skein scan-root remove /path/to/workspace
```

## Discovery contract

- Recursion is off unless `--recursive` is present.
- Default recursive depth is 16; maximum is 64.
- Directory symlinks are never followed.
- `.git` directories and worktree `.git` files identify repositories.
- A discovered repository is a traversal boundary.
- Common dependency, build, cache, vendor, and virtual-environment directories are
  pruned.
- Entry-level I/O errors are reported without aborting unrelated branches.
- An unavailable configured root retains its cached projects and can still be
  removed while unmounted.

## What `index` refreshes

```console
skein index
skein index --force
skein index --working-tree
skein index --project /path/to/repo
skein index --scan-root /path/to/workspace
```

The default path refreshes discovery, Git administrative metadata, project identity
documents, enabled context sources, and bounded Codex session metadata. `--force`
bypasses unchanged fingerprints. `--working-tree` additionally checks tracked-file
dirty state; untracked files and submodules remain excluded.

`--project` and `--scan-root` are mutually exclusive. Project scope validates one
registered project and performs no discovery. Scan-root scope validates one configured
root before traversal, discovers only that root, and refreshes only projects with
durable provenance from it. Other roots and sibling projects are untouched. If the
selected root is offline, cached project relationships are retained and the report
contains an `offline` deferral. Scoped runs defer context and Codex session refreshes
because those sources currently have global atomic replacement contracts.

The Git adapter does not fetch or contact remotes.

## Project identity documents

For each project, Skein selects at most 40 files and reads at most 64 KiB per file
and 512 KiB total. Preferred candidates are Git-tracked:

- README variants;
- `AGENTS.md`;
- common project manifests; and
- top-level Markdown beneath `docs/` and `.codex/`.

If `git ls-files` fails, a fixed bounded candidate set is used and may include
untracked files. Symlinks are never followed. The private FTS row changes only when
its fingerprint changes. Search exposes at most a bounded snippet and source paths,
not complete documents.

## Search layers

```console
skein search renderer session
skein search renderer session --limit 20
skein search old title --include-session-text
skein context search renderer
```

Normal project search combines canonical path/name, Git metadata, identity documents,
and optionally imported session titles/previews. Enabled context documents can
contribute bounded hits. `context search` addresses only the opted-in private context
index.

MCP callers must set `include_deep_context=true` before memory or raw-session snippets
can enter model context. See [context recall](context-recall.md).

## Slow and network-mounted workspaces

Language speed cannot hide slow disk and network metadata calls. Session Skein avoids
that I/O on the hot path:

- add the narrowest root that covers the repositories you need;
- keep recursive depth realistic;
- rely on repository boundaries and exclusions;
- omit `--force` during normal refreshes;
- omit `--working-tree` unless dirty state matters;
- run `skein index` once, then use SQLite-backed search and TUI views; and
- leave an unavailable root configured if it is temporarily offline—cached projects
  remain available.

For a very large mount, approve multiple targeted roots rather than one filesystem
root. The first recursive discovery can still be slow because every layer between
WSL, a network transport, and a physical disk must answer directory metadata calls.

## Freshness and partial failures

Document and context refreshes stage new rows and replace a source atomically.
Unavailable or file-cap-truncated sources retain the last complete rows and report a
deferred status. One project Git error is included in the index report rather than
silently discarded.

Human output labels the selected scope and deferrals. JSON output adds `scope` and
`deferred`; `refreshed` remains the CLI Git-report field and `reports` is retained for
MCP compatibility.

Inspect current state with:

```console
skein doctor
skein project show /path/to/project
skein context status
skein session list
```

## Machine-readable use

```console
skein --format json index
skein --format json search session skein
```

`--format json` is global and may appear before or after subcommands. Older local
`--json` flags remain accepted for compatibility. Streaming worker and control
events use `--jsonl`.
