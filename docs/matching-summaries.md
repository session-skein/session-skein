# Explainable matching and activity views

Alpha 4 adds a read-only recommendation layer. The query is private stdin data:

```console
printf '%s\n' 'continue the checkout failure investigation' | \
  skein match --limit 5 --json
```

Matching uses registered project name/path components, observed Git branch and latest
commit-subject tokens, linked session cwd, exact source-thread identity, optional
explicitly imported session text, fixed recency buckets, project-link provenance, and
recorded recovery-required state. Every integer contribution is returned as structured
evidence. Recency, linkage, and recovery state can strengthen a lexical or exact match;
they cannot create a candidate alone. Ties use stable metadata ordering.

Confidence is a deterministic label, not a probability. An exact canonical path is
high confidence. Exact project-name or source-thread evidence is high only with the
documented runner-up margin; duplicate identities remain low and ambiguous. Inferred
matches need documented score and runner-up margins. `match` always says
`dispatchable: false` and never starts, resumes, steers, or interrupts Codex. The
conductor evaluates the same ranking in an explicit dispatch context, where only a
unique high-confidence recommendation becomes dispatchable and is rechecked inside
the write transaction.

By default, synchronized session names and previews do not participate. `--include-text`
locally scores only text previously imported with explicit consent. Source values and
query tokens are not echoed in the evidence, and the query/result are never stored.

## Project cards

```console
skein summary project /path/to/registered/project
skein summary project /path/to/registered/project --json
skein summary projects --json
```

A project card is generated on read from the latest stored Git snapshot, linked-session
counts, and control-run states. It updates automatically after the underlying metadata
changes through `project refresh`, `session sync`, or control activity. No generated
prose cache, repository scan, Codex process, model, embedding, or network call exists.

The narrative is factual rather than semantic. It may quote the stored latest Git
commit subject, but it cannot claim what the project does, what Codex accomplished, or
why work stopped without an explicit future content source.

## Day digest

```console
skein summary day
skein summary day 2026-07-11 --json
```

The CLI converts the selected machine-local calendar day to exact Unix boundaries.
The core projects current rows into that interval: mutable latest session-observation
timestamps, latest source-update timestamps, control-run creation/terminal timestamps,
current recovery state, Git refresh observation time, and the latest commit's source
time. It does not read the control action-event ledger or reconstruct intermediate run
states. Working-tree state exists only after an explicit tracked-file refresh, and
external shell work/untracked files are not observed. This is therefore a bounded
current-metadata digest, not a claim to capture everything the user did.
