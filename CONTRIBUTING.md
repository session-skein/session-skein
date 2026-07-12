# Contributing

Session Skein is early. Before implementing a large adapter or UI, open a focused
proposal describing the user-visible behavior, source of truth, privacy impact, and
failure modes.

Every change must pass:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release --locked
cargo audit --deny warnings
```

Start at [the handbook](docs/index.md). Any user-visible command, MCP tool, environment
variable, state path, installer option, key binding, or trust-boundary change must
update its task guide and reference page in the same pull request. The documentation
contract tests reject missing command/tool coverage, broken local links, invalid
plugin/skill structure, version drift, and machine-specific public paths.

Use synthetic fixtures. Never commit transcripts, credentials, machine-specific
absolute paths, or copies of private repositories. Keep observation separate from
control, and make destructive or externally visible actions explicit.
