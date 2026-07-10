# Git metadata refresh

Session Skein observes registered repositories without recursively discovering them.
The refresh modes make their I/O cost and freshness guarantees explicit.

## Fast metadata mode

```console
skein project refresh /path/to/project --json
```

The first refresh reads the repository's Git directory and invokes local Git to record:

- short branch name, when attached;
- full `HEAD` object ID;
- latest commit timestamp and subject;
- refresh timestamp.

It stores a fingerprint derived from `HEAD`, its loose branch reference, the index
stamp, and the packed-reference stamp. When that fingerprint is unchanged, a later
refresh returns `"status": "unchanged"` without invoking Git. This mode does not
claim that working files are clean. Direct reads of Git administrative files are
capped at 64 KiB each.

## Tracked working-tree mode

```console
skein project refresh /path/to/project --working-tree --json
```

This mode always invokes a quiet Git comparison and records `tracked_dirty` as `true`
or `false`. It ignores untracked files and submodules. The stored value is a snapshot
from `metadata_refreshed_at`, not a live promise; a later file edit cannot be observed
without another `--working-tree` refresh.

## Scope and forcing

Refresh requires either one path or the explicit `--all` flag. All-project refreshes
run sequentially so slow roots do not create an uncontrolled I/O burst.

`--force` bypasses the stored fingerprint. It does not imply `--working-tree`.

No refresh mode fetches remotes, updates refs, changes the index, scans untracked
files, or stores diffs and commit bodies.

Standard repositories and linked Git worktrees are supported. Bare repositories are
not recognized as project worktrees in this release.
