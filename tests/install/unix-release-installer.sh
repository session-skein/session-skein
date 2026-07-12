#!/usr/bin/env bash
set -euo pipefail
umask 077

BINARY="${1:?usage: unix-release-installer.sh PATH_TO_SKEIN}"
REPO_ROOT="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
VERSION="$($BINARY --version | awk '{print $2}')"
case "$(uname -s):$(uname -m)" in
  Linux:x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
  Darwin:x86_64) TARGET="x86_64-apple-darwin" ;;
  Darwin:arm64) TARGET="aarch64-apple-darwin" ;;
  *) echo "unsupported test platform: $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac
TAG="v$VERSION"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT HUP INT TERM
RELEASE_ROOT="$ROOT/releases"
CHANNEL_ROOT="$ROOT/channels"
ASSET_DIR="$RELEASE_ROOT/$TAG"
mkdir -p "$ASSET_DIR" "$CHANNEL_ROOT"
printf '%s\n' "$VERSION" > "$CHANNEL_ROOT/preview"

sha_file() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'
  fi
}

set_manifest_hash() {
  python - "$ASSET_DIR/release-manifest.json" "$1" <<'PY'
import json, sys
path, digest = sys.argv[1:]
with open(path, encoding="utf-8") as handle:
    value = json.load(handle)
value["assets"][0]["sha256"] = digest
with open(path, "w", encoding="utf-8", newline="\n") as handle:
    json.dump(value, handle, separators=(",", ":"))
    handle.write("\n")
PY
}

python "$REPO_ROOT/scripts/release.py" package --binary "$BINARY" --target "$TARGET" --output "$ASSET_DIR" >/dev/null
ARCHIVE="session-skein-$TAG-$TARGET.tar.gz"
HASH="$(sha_file "$ASSET_DIR/$ARCHIVE")"
cp "$ASSET_DIR/$ARCHIVE" "$ROOT/good-archive"
cat > "$ASSET_DIR/release-manifest.json" <<EOF
{"schemaVersion":1,"name":"session-skein","version":"$VERSION","tag":"$TAG","assets":[{"name":"$ARCHIVE","target":"$TARGET","sha256":"$HASH"}]}
EOF
printf '%s  %s\n' "$HASH" "$ARCHIVE" > "$ASSET_DIR/SHA256SUMS"

run_remote() {
  case_root="$1"
  shift
  HOME="$case_root/home" XDG_STATE_HOME="$case_root/state" \
    SKEIN_DATA_DIR="$case_root/data" SKEIN_CONFIG_DIR="$case_root/config" \
    CODEX_HOME="$case_root/codex" SKEIN_BIN_DIR="$case_root/bin" \
    SKEIN_RELEASE_BASE_URL="file://$RELEASE_ROOT" \
    SKEIN_RELEASE_CHANNEL_URL="file://$CHANNEL_ROOT" \
    SKEIN_ALLOW_INSECURE_TEST_DOWNLOADS=1 \
    "$REPO_ROOT/install.sh" "$@"
}

# Default preview mode installs from the channel without Git or Rust, then safely
# reinstalls and uninstalls through the existing receipt ownership path.
clean="$ROOT/clean"
run_remote "$clean" --no-mcp >/dev/null
test "$("$clean/bin/skein" --version)" = "skein $VERSION"
grep -Fxq "source=release:$TAG:$TARGET" "$clean/state/session-skein/install/receipt"
test -f "$clean/codex/skills/session-skein/SKILL.md"
run_remote "$clean" --version "$VERSION" --no-mcp >/dev/null
run_remote "$clean" --uninstall >/dev/null
test ! -e "$clean/bin/skein"
test ! -e "$clean/codex/skills/session-skein"

# A checksum mismatch fails before extraction or destination mutation.
bad_checksum="$ROOT/bad-checksum"
cp "$ASSET_DIR/SHA256SUMS" "$ROOT/good-checksums"
printf '%064d  %s\n' 0 "$ARCHIVE" > "$ASSET_DIR/SHA256SUMS"
if run_remote "$bad_checksum" --version "$VERSION" --no-mcp >/dev/null 2>&1; then
  echo 'checksum mismatch unexpectedly installed' >&2
  exit 1
fi
test ! -e "$bad_checksum/bin/skein"
mv "$ROOT/good-checksums" "$ASSET_DIR/SHA256SUMS"

# A checksum-valid traversal archive is rejected before extraction.
traversal="$ROOT/traversal"
python - "$ASSET_DIR/$ARCHIVE" <<'PY'
import io, sys, tarfile
with tarfile.open(sys.argv[1], "w:gz") as archive:
    info = tarfile.TarInfo("../escape")
    info.size = 6
    archive.addfile(info, io.BytesIO(b"escape"))
PY
HASH="$(sha_file "$ASSET_DIR/$ARCHIVE")"
printf '%s  %s\n' "$HASH" "$ARCHIVE" > "$ASSET_DIR/SHA256SUMS"
set_manifest_hash "$HASH"
if run_remote "$traversal" --version "$VERSION" --no-skill --no-mcp >/dev/null 2>&1; then
  echo 'traversal archive unexpectedly installed' >&2
  exit 1
fi
test ! -e "$ROOT/escape"
cp "$ROOT/good-archive" "$ASSET_DIR/$ARCHIVE"
HASH="$(sha_file "$ASSET_DIR/$ARCHIVE")"
printf '%s  %s\n' "$HASH" "$ARCHIVE" > "$ASSET_DIR/SHA256SUMS"
set_manifest_hash "$HASH"

# The selected archive binary must report the release version even when all outer
# metadata and checksums are internally consistent.
mismatch="$ROOT/mismatch"
payload="$ROOT/payload"
mkdir -p "$payload"
tar -xzf "$ASSET_DIR/$ARCHIVE" -C "$payload"
payload_root="$payload/session-skein-$TAG-$TARGET"
cat > "$payload_root/skein" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = "--version" ]; then printf '%s\n' 'skein 0.0.0-preview'; exit 0; fi
if [ "${1:-}" = "--format" ]; then printf '%s\n' '{"version":"0.0.0-preview","config_dir":"x","data_dir":"y","database":"z"}'; exit 0; fi
exit 1
EOF
chmod 0755 "$payload_root/skein"
tar -czf "$ASSET_DIR/$ARCHIVE" -C "$payload" "session-skein-$TAG-$TARGET"
HASH="$(sha_file "$ASSET_DIR/$ARCHIVE")"
printf '%s  %s\n' "$HASH" "$ARCHIVE" > "$ASSET_DIR/SHA256SUMS"
set_manifest_hash "$HASH"
if run_remote "$mismatch" --version "$VERSION" --no-skill --no-mcp >/dev/null 2>&1; then
  echo 'binary version mismatch unexpectedly installed' >&2
  exit 1
fi
test ! -e "$mismatch/bin/skein"

# Unsupported OS/architecture combinations fail before any network request.
fakebin="$ROOT/fakebin"
mkdir -p "$fakebin"
cat > "$fakebin/uname" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in -s) echo FreeBSD ;; -m) echo riscv64 ;; *) echo FreeBSD ;; esac
EOF
chmod 0755 "$fakebin/uname"
if PATH="$fakebin:$PATH" run_remote "$ROOT/unsupported" --version "$VERSION" --no-skill --no-mcp >"$ROOT/unsupported.out" 2>&1; then
  echo 'unsupported platform unexpectedly installed' >&2
  exit 1
fi
grep -Fq 'unsupported release platform: FreeBSD/riscv64' "$ROOT/unsupported.out"

printf '%s\n' 'Unix binary-first release installer tests passed.'
