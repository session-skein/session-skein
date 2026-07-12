# Getting started

This guide takes a fresh installation to a searchable project library and a working
Codex conductor. Complete [installation](../INSTALL.md) first.

## 1. Verify the local runtime

```console
skein --version
skein doctor
codex login status
```

`doctor` is strictly read-only and never creates or migrates the database. `init`
creates private state or applies forward-only schema migrations:

```console
skein init
```

## 2. Choose how projects enter the catalog

For one repository, register its exact path:

```console
skein project add /path/to/repository
```

For a workspace containing several repositories, approve a scan root. Exact-root
mode checks only whether that directory is itself a repository:

```console
skein scan-root add /path/to/workspace
```

Recursive discovery is opt-in:

```console
skein scan-root add /path/to/workspace --recursive --max-depth 16
```

Discovery never follows directory symlinks. A discovered Git repository is a
boundary, so Skein does not walk every source directory inside it. Common build,
dependency, cache, vendor, and virtual-environment directories are pruned.

Review the authorization:

```console
skein scan-root list
skein project list
```

## 3. Build the local indexes

```console
skein index
```

One index run performs bounded work in this order:

1. discover projects beneath approved scan roots;
2. refresh incremental Git metadata;
3. refresh bounded project identity documents;
4. refresh enabled private context sources; and
5. synchronize bounded, content-free Codex session metadata.

An unavailable network root is reported and its cached projects are retained. One
project failure does not erase successful refreshes for other projects.

## 4. Search and recover existing work

```console
skein search session skein
skein session list
skein summary projects
skein summary day
```

Search covers project identity and Git metadata by default. Generated Codex memories
and raw user/assistant session messages are separate, defaults-off sources. Read
[context recall](context-recall.md) before opting in.

If you want an explicit full session import:

```console
skein session sync codex --all-pages
```

This stores thread metadata, not turns or transcripts. Unmatched sessions remain
visible and can be bound deliberately:

```console
skein session bind THREAD_ID /path/to/registered/project
```

## 5. Open the project library

```console
skein tui
```

The TUI loads the local catalog immediately and refreshes slow sources in the
background. Press `?` for keys. A conductor dispatch requires `F2` immediately before
`Enter`; the authorization is consumed once.

## 6. Use the conductor from a shell

The prompt comes from standard input and is never stored by Session Skein:

```console
printf '%s\n' 'continue the renderer investigation' | \
  skein conduct --full-access --follow
```

Skein dispatches only when one registered project has a unique high-confidence
route. Ambiguity is a result to resolve, not permission to guess.

## 7. Use it from Codex

Installation registers the MCP server. Start a new Codex session and ask naturally:

> Find the project where I was working on the renderer and show the relevant
> sessions.

> Conduct this task in the uniquely matching project: run its focused tests.

The bundled `session-skein` skill teaches Codex to search before guessing, inspect an
existing run before starting another, and preserve the privacy/control gates.

## Next

- [Understand the model](concepts.md)
- [Tune indexing for a network disk](indexing-and-search.md#slow-and-network-mounted-workspaces)
- [Enable private context deliberately](context-recall.md)
- [Learn worker recovery](workers.md)
- [See every command](cli-reference.md)
