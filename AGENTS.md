# Agent development guide

Keep Session Skein local-first, adapter-driven, and independent from any single agent
vendor. Start at `docs/index.md`. Read `docs/architecture.md`, `docs/privacy.md`, and
`INSTALL.md` before changing state, integration, installer, plugin, or skill behavior.

Required checks:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo audit
```

The test suite includes the documentation, installer, skill, plugin, public-path, and
command/tool drift contract. Update task guides and reference pages in the same change
as user-visible behavior. Validate the bundled skill and plugin with the current
Codex `skill-creator` and `plugin-creator` validators when those tools are available.

When a user asks to install this repository, follow `INSTALL.md` as the normative
contract. Determine whether they requested the catalog-only or control-enabled MCP
profile, run the matching installer, and verify the installed executable, `doctor`,
MCP registration, and active Codex skill path. Do not add a scan root, enable private
context, start a worker, or install a service unless the user separately asks for it.

Do not add machine-specific paths, real transcript fragments, credentials, or personal
identifiers to code, fixtures, documentation, commits, or issue examples. Tests must
use temporary directories and synthetic data. Observation must remain separate from
control; any new external-state mutation needs a visible policy boundary and an audit
record.
