# Preview releases

Session Skein preview releases provide native command-line packages for:

| Asset target | GitHub-hosted builder | Archive |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | Ubuntu 24.04 x86_64 | `.tar.gz` |
| `x86_64-apple-darwin` | macOS 15 Intel | `.tar.gz` |
| `aarch64-apple-darwin` | macOS 15 arm64 | `.tar.gz` |
| `x86_64-pc-windows-msvc` | Windows Server 2025 x86_64 | `.zip` |

These target triples are maintained Rust platforms, and the workflow uses native
GitHub-hosted runners rather than cross-compilation. See the primary
[Rust platform-support table](https://doc.rust-lang.org/rustc/platform-support.html)
and [GitHub-hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

## Release contents

Every platform archive has one versioned top-level directory containing:

- `skein` or `skein.exe`;
- `README.md` and `LICENSE` from the same tagged revision;
- the matching `plugin/` metadata, MCP declaration, skill, agent metadata, and skill
  references; and
- `release-package.json`, which records the version, target, file sizes, and SHA-256
  hashes of packaged inputs.

The release also includes `release-manifest.json`, describing every platform asset,
and `SHA256SUMS`, covering all archives and the release manifest. Archive ordering,
timestamps, owners, permissions, and compression settings are fixed so repackaging
the same inputs on the same supported Python runtime produces identical bytes.

## Verification

Download the archive, `release-manifest.json`, and `SHA256SUMS` from the same GitHub
Release. Verify checksums on Linux or macOS:

```console
sha256sum --check SHA256SUMS
```

On macOS, `shasum -a 256` can be compared with the matching line. On Windows:

```powershell
Get-FileHash -Algorithm SHA256 .\session-skein-*.zip
```

GitHub also records build-provenance attestations for every published asset. With
GitHub CLI installed, verify an asset against the canonical repository:

```console
gh attestation verify session-skein-v0.5.0-alpha.10-x86_64-unknown-linux-gnu.tar.gz \
  --repo session-skein/session-skein
```

Artifact attestations establish which GitHub Actions workflow and repository produced
an asset; they are not a substitute for platform code signing. See GitHub's primary
[artifact-attestation documentation](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations).

## Publication contract

Pull requests build and validate all four packages, assemble the manifest, and check
the checksum set using read-only repository permissions. They cannot publish or create
attestations. A push of a `v`-prefixed semantic version tag may publish only when the
tag is exactly `v` plus the workspace/plugin version. The workflow revalidates that
identity after all builds, attests the complete local asset set, creates a draft
prerelease, uploads every asset, and only then publishes the draft.

## Preview limitations

The preview binaries are currently unsigned. macOS packages are not signed or
notarized, and Windows packages do not carry an Authenticode signature. Gatekeeper or
SmartScreen may therefore warn or block execution depending on local policy. Verify
checksums and provenance before making a deliberate local trust decision. Native
macOS signing/notarization and Windows signing remain roadmap work.

The normal installers are binary-first. They intentionally resolve the repository's
`preview` channel pointer because GitHub's “latest release” endpoint excludes
prereleases, then pin all downloads to the resolved tag. They require the expected
archive to appear exactly once in both the release manifest and checksums, verify its
SHA-256 before extraction, reject unsafe archive paths/links, and require the binary,
package metadata, and bundled plugin version to agree.

Use `--version` or `-Version` for an exact preview. The default repository and asset
URLs are fixed to `session-skein/session-skein`. `SKEIN_RELEASE_BASE_URL` and
`SKEIN_RELEASE_CHANNEL_URL` exist only for hermetic tests and require the separate
`SKEIN_ALLOW_RELEASE_OVERRIDE=1` gate. Non-HTTPS fixtures additionally require
`SKEIN_ALLOW_INSECURE_TEST_DOWNLOADS=1`. None are supported for normal installation.

Release archives include matching Unix and PowerShell installers. Alpha.9 stores the
appropriate verified installer as a hashed private snapshot, enabling the CLI-only
`skein update` command to reuse the same trust and rollback contract. There is no
background update, and update is deliberately absent from MCP.

Because alpha.8 was immutable before this command existed, alpha.8 users run the
binary-first installer once to reach alpha.9. Subsequent previews use `skein update`.
The first honest cross-release product update is therefore alpha.9 to alpha.10 and
will be exercised by that release's native CI. Alpha.9 CI exercises the same updater
and installer mechanism honestly through forced same-version reinstall, including
the native Windows parent-exit and locked-executable handoff; hermetic tests also
cover release inspection, downgrade policy, drift refusal, and injected transactional
rollback without falsifying a binary's current version.
