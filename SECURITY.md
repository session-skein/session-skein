# Security policy

Session Skein is pre-release software and should not yet be treated as a security
boundary. Do not expose its state database, worker capability files, authenticated
loopback IPC, or MCP stdio transport to a network or another user.

Please report suspected vulnerabilities privately through GitHub's security advisory
feature once the canonical repository is published. Do not include real transcripts,
credentials, private repository paths, or other personal data in a public issue.

Only the latest released version receives security fixes during the pre-1.0 period.

| Version | Supported |
| --- | --- |
| `0.5.0-alpha.8` | Yes |
| Earlier previews | No |

Preview release assets include SHA-256 checksums and GitHub artifact attestations,
but the executables are not code-signed. macOS builds are not notarized and Windows
builds do not carry an Authenticode signature. Verify provenance and checksums before
execution. Platform signing and notarization remain planned work.

The binary-first installers are part of the release supply-chain boundary. Their
default URLs are pinned to the canonical repository, preview resolution produces an
exact version before asset download, and archives are verified before extraction.
Repository/channel URL overrides are intended only for controlled mirrors and tests;
using one transfers trust to that endpoint.
