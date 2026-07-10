# Contributing

Session Skein is early. Before implementing a large adapter or UI, open a focused
proposal describing the user-visible behavior, source of truth, privacy impact, and
failure modes.

Every change must pass:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Use synthetic fixtures. Never commit transcripts, credentials, machine-specific
absolute paths, or copies of private repositories. Keep observation separate from
control, and make destructive or externally visible actions explicit.
