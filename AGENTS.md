# Agent development guide

Keep Session Skein local-first, adapter-driven, and independent from any single agent
vendor. Read `docs/architecture.md` and `docs/privacy.md` before changing state or
integration behavior.

Required checks:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo audit
```

Do not add machine-specific paths, real transcript fragments, credentials, or personal
identifiers to code, fixtures, documentation, commits, or issue examples. Tests must
use temporary directories and synthetic data. Observation must remain separate from
control; any new external-state mutation needs a visible policy boundary and an audit
record.
