#!/usr/bin/env bash

# Session Skein installer for Linux and macOS.
# It verifies one published release or builds an explicit source revision, preflights
# every destination, and records hashes so
# uninstall never deletes a user-replaced binary or integration.

set -euo pipefail
umask 077

ORIGINAL_ARGS=("$@")
REPO_URL="${SKEIN_REPO_URL:-https://github.com/session-skein/session-skein.git}"
RELEASE_BASE_URL="${SKEIN_RELEASE_BASE_URL:-https://github.com/session-skein/session-skein/releases/download}"
RELEASE_CHANNEL_URL="${SKEIN_RELEASE_CHANNEL_URL:-https://raw.githubusercontent.com/session-skein/session-skein/main/release-channels}"
[ -z "${SKEIN_RELEASE_BASE_URL:-}${SKEIN_RELEASE_CHANNEL_URL:-}" ] || \
  [ "${SKEIN_ALLOW_RELEASE_OVERRIDE:-0}" = "1" ] || \
  { printf 'error: release endpoint overrides require SKEIN_ALLOW_RELEASE_OVERRIDE=1 (testing only)\n' >&2; exit 1; }
PROFILE="catalog"
PROFILE_FLAGS=0
BINARY_SOURCE=""
SOURCE_OVERRIDE=""
BIN_DIR="${SKEIN_BIN_DIR:-${HOME:?HOME is required}/.local/bin}"
NO_MCP=0
NO_SKILL=0
REPLACE_BINARY=0
REPLACE_MCP=0
REPLACE_SKILL=0
UPDATE=0
UNINSTALL=0
CHECK_ONLY=0
JSON_OUTPUT=0
RELEASE_VERSION=""
RELEASE_CHANNEL="preview"
RESOLVED_FROM_CHANNEL=0
DOWNLOAD_DIR=""

cleanup() {
  if [ -n "$DOWNLOAD_DIR" ] && [ -d "$DOWNLOAD_DIR" ]; then
    rm -rf -- "$DOWNLOAD_DIR"
  fi
}
trap cleanup EXIT HUP INT TERM

usage() {
  cat <<'USAGE'
Session Skein installer

Usage:
  ./install.sh [options]
  curl -fsSL https://raw.githubusercontent.com/session-skein/session-skein/main/install.sh |
    bash -s -- [options]

Options:
  --catalog-only     Register read/catalog MCP tools only (default)
  --control          Expose audited conduct, steer, interrupt, and reconcile tools
  --binary PATH      Install an already-built skein executable
  --source PATH      Explicitly build and install a Session Skein checkout
  --version VERSION  Install one exact published version
  --channel preview  Resolve the newest approved preview (default)
  --bin-dir PATH     Install the executable here (default: ~/.local/bin)
  --replace-binary   Back up and replace an unowned destination binary
  --no-mcp           Do not change MCP configuration
  --no-skill         Do not change the Codex skill
  --replace-mcp      Replace a conflicting MCP entry (a JSON backup is retained)
  --replace-skill    Back up and replace a conflicting skill path
  --update           Update an explicit Git source checkout and reinstall
  --uninstall        Remove only hash/target-matched installer-owned integration
  --check            Verify release availability without installing
  --json             Emit machine-readable check output
  -h, --help         Show this help

Environment:
  SKEIN_REPO_URL       Override the Git clone URL
  SKEIN_RELEASE_BASE_URL    Override the release asset base (testing only)
  SKEIN_RELEASE_CHANNEL_URL Override the channel-file base (testing only)
  SKEIN_INSTALL_SOURCE Override the managed checkout path
  SKEIN_BIN_DIR        Override the default binary directory
  CODEX_HOME           Override the Codex home (default: ~/.codex)
  SKEIN_CONFIG_DIR     Override Skein config and persist it into MCP configuration
  SKEIN_DATA_DIR       Override Skein data and persist it into MCP configuration
  SKEIN_CODEX_BIN      Override Codex runtime and persist it into MCP configuration
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

note() {
  printf '→ %s\n' "$*"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --catalog-only)
      PROFILE="catalog"
      PROFILE_FLAGS=$((PROFILE_FLAGS + 1))
      ;;
    --control)
      PROFILE="control"
      PROFILE_FLAGS=$((PROFILE_FLAGS + 1))
      ;;
    --binary)
      [ "$#" -ge 2 ] || die "--binary requires a path"
      BINARY_SOURCE="$2"
      shift
      ;;
    --source)
      [ "$#" -ge 2 ] || die "--source requires a path"
      SOURCE_OVERRIDE="$2"
      shift
      ;;
    --version)
      [ "$#" -ge 2 ] || die "--version requires a version"
      RELEASE_VERSION="$2"
      shift
      ;;
    --channel)
      [ "$#" -ge 2 ] || die "--channel requires a name"
      RELEASE_CHANNEL="$2"
      shift
      ;;
    --bin-dir)
      [ "$#" -ge 2 ] || die "--bin-dir requires a path"
      BIN_DIR="$2"
      shift
      ;;
    --replace-binary) REPLACE_BINARY=1 ;;
    --no-mcp) NO_MCP=1 ;;
    --no-skill) NO_SKILL=1 ;;
    --replace-mcp) REPLACE_MCP=1 ;;
    --replace-skill) REPLACE_SKILL=1 ;;
    --update) UPDATE=1 ;;
    --uninstall) UNINSTALL=1 ;;
    --check) CHECK_ONLY=1 ;;
    --json) JSON_OUTPUT=1 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1 (run --help)" ;;
  esac
  shift
done

[ "$PROFILE_FLAGS" -le 1 ] || die "choose either --catalog-only or --control"
[ -z "$BINARY_SOURCE" ] || [ -z "$SOURCE_OVERRIDE" ] || \
  die "--binary and --source are mutually exclusive"
[ -z "$BINARY_SOURCE" ] || [ -z "$RELEASE_VERSION" ] || \
  die "--binary and --version are mutually exclusive"
[ -z "$SOURCE_OVERRIDE" ] || [ -z "$RELEASE_VERSION" ] || \
  die "--source and --version are mutually exclusive"
[ "$RELEASE_CHANNEL" = "preview" ] || die "unsupported channel: $RELEASE_CHANNEL (expected preview)"

CODEX_HOME_DIR="${CODEX_HOME:-$HOME/.codex}"
INSTALL_STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/session-skein/install"
RECEIPT="$INSTALL_STATE_DIR/receipt"
MCP_BACKUP="$INSTALL_STATE_DIR/replaced-mcp.json"
MCP_ROLLBACK="$INSTALL_STATE_DIR/codex-config.rollback"
MCP_JSON_ROLLBACK="$INSTALL_STATE_DIR/mcp.rollback.json"
RECEIPT_ROLLBACK="$INSTALL_STATE_DIR/receipt.rollback"
BINARY_ROLLBACK="$INSTALL_STATE_DIR/binary.rollback"
INSTALLER_ROLLBACK="$INSTALL_STATE_DIR/installer.rollback"
CODEX_CONFIG_FILE="$CODEX_HOME_DIR/config.toml"
SKILL_SNAPSHOT_ROOT="$INSTALL_STATE_DIR/skills"
MANAGED_SOURCE="${SKEIN_INSTALL_SOURCE:-${XDG_DATA_HOME:-$HOME/.local/share}/session-skein/repo}"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" 2>/dev/null && pwd -P || true)"
SOURCE_DIR=""
SOURCE_COMMIT=""
SOURCE_RECEIPT=""
PLUGIN_DIR=""
INSTALLED_BINARY="$BIN_DIR/skein"
INSTALLER_SNAPSHOT="$INSTALL_STATE_DIR/installer.sh"

receipt_value() {
  key="$1"
  [ -f "$RECEIPT" ] || return 0
  sed -n "s/^${key}=//p" "$RECEIPT" | head -n 1
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die "sha256sum or shasum is required"
  fi
}

sha256_text() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  else
    die "sha256sum or shasum is required"
  fi
}

sha256_tree() {
  tree_root="$1"
  (
    CDPATH= cd -- "$tree_root"
    find . \( -type f -o -type l \) -print | LC_ALL=C sort | while IFS= read -r entry; do
      printf '%s\n' "$entry"
      if [ -L "$entry" ]; then
        printf 'link:%s\n' "$(readlink "$entry")"
      else
        sha256_file "$entry"
      fi
    done
  ) | sha256_text
}

codex_mcp_json() {
  NO_COLOR=1 FORCE_COLOR=0 codex mcp get session-skein --json 2>/dev/null || true
}

uninstall_owned() {
  [ -f "$RECEIPT" ] || die "no installer receipt found at $RECEIPT"
  preserved=0

  owned_mcp_hash="$(receipt_value mcp_hash)"
  if [ -n "$owned_mcp_hash" ]; then
    if command -v codex >/dev/null 2>&1; then
      current_mcp="$(codex_mcp_json)"
    else
      current_mcp=""
    fi
    if [ -n "$current_mcp" ]; then
      current_mcp_hash="$(printf '%s' "$current_mcp" | sha256_text)"
      if [ "$current_mcp_hash" = "$owned_mcp_hash" ]; then
        note "Removing installer-owned Codex MCP registration"
        NO_COLOR=1 FORCE_COLOR=0 codex mcp remove session-skein >/dev/null
      else
        printf '• Preserving modified session-skein MCP registration.\n' >&2
        preserved=1
      fi
    else
      printf '• Could not verify the installer-owned MCP registration; preserving its receipt.\n' >&2
      preserved=1
    fi
  fi

  owned_skill="$(receipt_value skill)"
  skill_source="$(receipt_value skill_source)"
  skill_backup="$(receipt_value skill_backup)"
  if [ -n "$owned_skill" ]; then
    if [ -L "$owned_skill" ] && [ "$(readlink "$owned_skill" || true)" = "$skill_source" ]; then
      note "Removing installer-owned skill link"
      rm -f "$owned_skill"
      if [ -n "$skill_backup" ] && [ -e "$skill_backup" ]; then
        mkdir -p "$(dirname -- "$owned_skill")"
        mv "$skill_backup" "$owned_skill"
        note "Restored the previous skill path"
      fi
    elif [ -e "$owned_skill" ] || [ -L "$owned_skill" ]; then
      printf '• Preserving modified skill path %s.\n' "$owned_skill" >&2
      preserved=1
    elif [ -n "$skill_backup" ] && [ -e "$skill_backup" ]; then
      mkdir -p "$(dirname -- "$owned_skill")"
      mv "$skill_backup" "$owned_skill"
      note "Restored the previous skill path"
    fi
  fi

  owned_binary="$(receipt_value binary)"
  owned_binary_hash="$(receipt_value binary_hash)"
  binary_backup="$(receipt_value binary_backup)"
  if [ -n "$owned_binary" ]; then
    if [ -f "$owned_binary" ] && \
       [ "$(sha256_file "$owned_binary")" = "$owned_binary_hash" ]; then
      note "Removing installer-owned binary $owned_binary"
      rm -f "$owned_binary"
      if [ -n "$binary_backup" ] && [ -e "$binary_backup" ]; then
        mkdir -p "$(dirname -- "$owned_binary")"
        mv "$binary_backup" "$owned_binary"
        note "Restored the previous destination binary"
      fi
    elif [ -e "$owned_binary" ]; then
      printf '• Preserving modified binary %s.\n' "$owned_binary" >&2
      preserved=1
    elif [ -n "$binary_backup" ] && [ -e "$binary_backup" ]; then
      mkdir -p "$(dirname -- "$owned_binary")"
      mv "$binary_backup" "$owned_binary"
      note "Restored the previous destination binary"
    fi
  fi

  owned_installer="$(receipt_value installer)"
  owned_installer_hash="$(receipt_value installer_hash)"
  if [ -n "$owned_installer" ]; then
    if [ -f "$owned_installer" ] && [ "$(sha256_file "$owned_installer")" = "$owned_installer_hash" ]; then
      rm -f "$owned_installer"
    elif [ -e "$owned_installer" ]; then
      printf '• Preserving modified installer snapshot %s.\n' "$owned_installer" >&2
      preserved=1
    fi
  fi

  if [ "$preserved" -eq 0 ]; then
    rm -f "$RECEIPT"
    printf '\n✓ Session Skein integration removed.\n'
  else
    printf '\nSession Skein preserved modified paths; the receipt remains at %s.\n' "$RECEIPT" >&2
  fi
  printf 'Private data and the source checkout were preserved.\n'
  [ -e "$MCP_BACKUP" ] && \
    printf 'A replaced MCP JSON backup remains at %s.\n' "$MCP_BACKUP"
  return 0
}

if [ "$UNINSTALL" -eq 1 ]; then
  uninstall_owned
  exit 0
fi

PREVIOUS_BINARY="$(receipt_value binary)"
PREVIOUS_BINARY_HASH="$(receipt_value binary_hash)"
PREVIOUS_BINARY_BACKUP="$(receipt_value binary_backup)"
PREVIOUS_SKILL="$(receipt_value skill)"
PREVIOUS_SKILL_SOURCE="$(receipt_value skill_source)"
PREVIOUS_SKILL_BACKUP="$(receipt_value skill_backup)"
PREVIOUS_MCP_PROFILE="$(receipt_value mcp_profile)"
PREVIOUS_MCP_HASH="$(receipt_value mcp_hash)"
PREVIOUS_MCP_SPEC_HASH="$(receipt_value mcp_spec_hash)"
PREVIOUS_INSTALLER="$(receipt_value installer)"
PREVIOUS_INSTALLER_HASH="$(receipt_value installer_hash)"

resolve_source() {
  if [ -n "$SOURCE_OVERRIDE" ]; then
    SOURCE_DIR="$(CDPATH= cd -- "$SOURCE_OVERRIDE" 2>/dev/null && pwd -P)" || \
      die "source directory does not exist: $SOURCE_OVERRIDE"
  elif [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/Cargo.toml" ] && \
       [ -f "$SCRIPT_DIR/plugins/session-skein/skills/session-skein/SKILL.md" ]; then
    SOURCE_DIR="$SCRIPT_DIR"
  else
    SOURCE_DIR="$MANAGED_SOURCE"
  fi

  if [ ! -f "$SOURCE_DIR/Cargo.toml" ]; then
    command -v git >/dev/null 2>&1 || die "git is required to obtain Session Skein"
    note "Cloning Session Skein into $SOURCE_DIR"
    mkdir -p "$(dirname -- "$SOURCE_DIR")"
    git clone --depth 1 "$REPO_URL" "$SOURCE_DIR"
  fi

  if [ "$UPDATE" -eq 1 ] && [ "${SKEIN_UPDATE_REEXEC:-0}" != "1" ]; then
    [ -d "$SOURCE_DIR/.git" ] || die "--update requires a Git checkout"
    note "Updating $SOURCE_DIR"
    git -C "$SOURCE_DIR" pull --ff-only
    [ -x "$SOURCE_DIR/install.sh" ] || chmod 0755 "$SOURCE_DIR/install.sh"
    exec env SKEIN_UPDATE_REEXEC=1 "$SOURCE_DIR/install.sh" "${ORIGINAL_ARGS[@]}"
  fi

  [ -f "$SOURCE_DIR/plugins/session-skein/skills/session-skein/SKILL.md" ] || \
    die "source is missing the bundled Session Skein skill: $SOURCE_DIR"
  PLUGIN_DIR="$SOURCE_DIR/plugins/session-skein"
  if [ -d "$SOURCE_DIR/.git" ]; then
    SOURCE_COMMIT="$(git -C "$SOURCE_DIR" rev-parse HEAD 2>/dev/null || true)"
  fi
}

detect_release_target() {
  os="$(uname -s 2>/dev/null || true)"
  arch="$(uname -m 2>/dev/null || true)"
  case "$os:$arch" in
    Linux:x86_64|Linux:amd64) printf '%s\n' x86_64-unknown-linux-gnu ;;
    Darwin:x86_64|Darwin:amd64) printf '%s\n' x86_64-apple-darwin ;;
    Darwin:arm64|Darwin:aarch64) printf '%s\n' aarch64-apple-darwin ;;
    *) die "unsupported release platform: ${os:-unknown}/${arch:-unknown}; supported: Linux x86_64, macOS x86_64, macOS arm64" ;;
  esac
}

download_file() {
  url="$1"
  destination="$2"
  command -v curl >/dev/null 2>&1 || die "curl is required for binary installation"
  case "$url" in
    https://*) curl --proto '=https' --tlsv1.2 -fsSL --retry 3 --retry-all-errors \
      "$url" -o "$destination" || die "download failed: $url" ;;
    *) [ "${SKEIN_ALLOW_INSECURE_TEST_DOWNLOADS:-0}" = "1" ] || \
         die "refusing non-HTTPS release URL: $url"
       curl -fsSL "$url" -o "$destination" || die "download failed: $url" ;;
  esac
}

resolve_release() {
  target="$(detect_release_target)"
  command -v tar >/dev/null 2>&1 || die "tar is required for binary installation"
  DOWNLOAD_DIR="$(mktemp -d)"
  if [ -z "$RELEASE_VERSION" ]; then
    channel_file="$DOWNLOAD_DIR/channel"
    download_file "$RELEASE_CHANNEL_URL/$RELEASE_CHANNEL" "$channel_file"
    RELEASE_VERSION="$(tr -d '[:space:]' < "$channel_file")"
    RESOLVED_FROM_CHANNEL=1
  fi
  if [[ ! "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
    die "invalid release version: $RELEASE_VERSION"
  fi
  [ "$RESOLVED_FROM_CHANNEL" -eq 0 ] || [[ "$RELEASE_VERSION" == *-* ]] || \
    die "preview channel resolved a non-preview version: $RELEASE_VERSION"
  tag="v$RELEASE_VERSION"
  archive="session-skein-$tag-$target.tar.gz"
  release_url="$RELEASE_BASE_URL/$tag"
  note "Downloading Session Skein $RELEASE_VERSION for $target"
  download_file "$release_url/release-manifest.json" "$DOWNLOAD_DIR/release-manifest.json"
  download_file "$release_url/SHA256SUMS" "$DOWNLOAD_DIR/SHA256SUMS"
  grep -Eq "\"version\"[[:space:]]*:[[:space:]]*\"$RELEASE_VERSION\"" "$DOWNLOAD_DIR/release-manifest.json" || \
    die "release manifest version does not match $RELEASE_VERSION"
  grep -Eq "\"tag\"[[:space:]]*:[[:space:]]*\"$tag\"" "$DOWNLOAD_DIR/release-manifest.json" || \
    die "release manifest tag does not match $tag"
  grep -Eq "\"name\"[[:space:]]*:[[:space:]]*\"$archive\"" "$DOWNLOAD_DIR/release-manifest.json" || \
    die "release manifest does not contain $archive"
  [ "$(awk -v name="$archive" 'index($0, "\"name\"") && index($0, "\"" name "\"") { count++ } END { print count + 0 }' "$DOWNLOAD_DIR/release-manifest.json")" -eq 1 ] || \
    die "release manifest does not select exactly one $archive asset"
  manifest_hash="$(awk -v name="$archive" '
    index($0, "\"name\"") && index($0, "\"" name "\"") { selected=1 }
    selected && index($0, "\"sha256\"") {
      value=$0
      sub(/^.*"sha256"[[:space:]]*:[[:space:]]*"/, "", value)
      sub(/".*/, "", value)
      print value
      exit
    }
    selected && index($0, "}") { exit }
  ' "$DOWNLOAD_DIR/release-manifest.json")"
  expected_hash="$(awk -v name="$archive" '$2 == name { print $1 }' "$DOWNLOAD_DIR/SHA256SUMS")"
  [[ "$expected_hash" =~ ^[0-9a-fA-F]{64}$ ]] || \
    die "SHA256SUMS does not contain exactly one valid hash for $archive"
  [ "$(awk -v name="$archive" '$2 == name { count++ } END { print count + 0 }' "$DOWNLOAD_DIR/SHA256SUMS")" -eq 1 ] || \
    die "SHA256SUMS contains duplicate entries for $archive"
  [ "$(printf '%s' "$manifest_hash" | tr 'A-F' 'a-f')" = "$(printf '%s' "$expected_hash" | tr 'A-F' 'a-f')" ] || \
    die "release manifest and SHA256SUMS disagree for $archive"
  download_file "$release_url/$archive" "$DOWNLOAD_DIR/$archive"
  [ "$(sha256_file "$DOWNLOAD_DIR/$archive")" = "$(printf '%s' "$expected_hash" | tr 'A-F' 'a-f')" ] || \
    die "checksum verification failed for $archive"

  while IFS= read -r entry; do
    case "$entry" in
      /*|../*|*/../*|*/..) die "release archive contains an unsafe path: $entry" ;;
    esac
  done < <(tar -tzf "$DOWNLOAD_DIR/$archive")
  if tar -tvzf "$DOWNLOAD_DIR/$archive" | awk 'substr($1,1,1) != "-" && substr($1,1,1) != "d" { found=1 } END { exit !found }'; then
    die "release archive contains a link or special-file entry"
  fi
  tar -xzf "$DOWNLOAD_DIR/$archive" -C "$DOWNLOAD_DIR"
  SOURCE_DIR="$DOWNLOAD_DIR/session-skein-$tag-$target"
  [ -d "$SOURCE_DIR" ] || die "release archive has no expected top-level directory"
  [ ! -L "$SOURCE_DIR/skein" ] && [ -f "$SOURCE_DIR/skein" ] || die "release archive has no regular skein executable"
  [ -f "$SOURCE_DIR/plugin/.codex-plugin/plugin.json" ] || die "release archive has no plugin metadata"
  [ -f "$SOURCE_DIR/plugin/skills/session-skein/SKILL.md" ] || die "release archive has no bundled skill"
  if find "$SOURCE_DIR" -type l -print -quit | grep -q .; then
    die "release archive extracted a symbolic link"
  fi
  sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$SOURCE_DIR/release-package.json" | grep -Fxq "$RELEASE_VERSION" || \
    die "release package version does not match $RELEASE_VERSION"
  grep -Eq "\"target\"[[:space:]]*:[[:space:]]*\"$target\"" "$SOURCE_DIR/release-package.json" || \
    die "release package target does not match $target"
  BINARY_SOURCE="$SOURCE_DIR/skein"
  PLUGIN_DIR="$SOURCE_DIR/plugin"
  chmod 0755 "$BINARY_SOURCE"
  SOURCE_RECEIPT="release:$tag:$target"
}

if [ -n "$SOURCE_OVERRIDE" ] || [ "$UPDATE" -eq 1 ]; then
  resolve_source
  SOURCE_RECEIPT="$SOURCE_DIR"
elif [ -z "$BINARY_SOURCE" ]; then
  resolve_release
elif [ "$NO_SKILL" -eq 0 ]; then
  resolve_source
  SOURCE_RECEIPT="$SOURCE_DIR"
fi

if [ -z "$BINARY_SOURCE" ]; then
  command -v cargo >/dev/null 2>&1 || die "Rust 1.95+ is required for source installation"
  case "$(uname -s 2>/dev/null || true)" in
    Darwin)
      command -v xcrun >/dev/null 2>&1 && xcrun --find clang >/dev/null 2>&1 || \
        die "Xcode Command Line Tools are required; run: xcode-select --install"
      ;;
    *)
      if ! command -v cc >/dev/null 2>&1 && \
         ! command -v gcc >/dev/null 2>&1 && \
         ! command -v clang >/dev/null 2>&1; then
        die "a native C compiler/linker toolchain is required (for example, build-essential)"
      fi
      ;;
  esac
  note "Building the locked source checkout"
  cargo build --manifest-path "$SOURCE_DIR/Cargo.toml" --workspace --release --locked \
    --target-dir "$SOURCE_DIR/target"
  BINARY_SOURCE="$SOURCE_DIR/target/release/skein"
else
  BINARY_SOURCE="$(CDPATH= cd -- "$(dirname -- "$BINARY_SOURCE")" 2>/dev/null && pwd -P)/$(basename -- "$BINARY_SOURCE")" || \
    die "binary path does not exist: $BINARY_SOURCE"
fi
[ -f "$BINARY_SOURCE" ] || die "binary does not exist: $BINARY_SOURCE"

VERSION_OUTPUT="$($BINARY_SOURCE --version 2>/dev/null || true)"
if [[ "$VERSION_OUTPUT" =~ ^skein\ ([0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?)$ ]]; then
  ACTUAL_VERSION="${BASH_REMATCH[1]}"
else
  die "--binary did not identify itself as 'skein VERSION'"
fi
[ -z "$RELEASE_VERSION" ] || [ "$ACTUAL_VERSION" = "$RELEASE_VERSION" ] || \
  die "downloaded binary reports $ACTUAL_VERSION, expected $RELEASE_VERSION"
VALIDATION_DIR="$(mktemp -d)"
DOCTOR_JSON="$(SKEIN_CONFIG_DIR="$VALIDATION_DIR/config" \
  SKEIN_DATA_DIR="$VALIDATION_DIR/data" "$BINARY_SOURCE" --format json doctor 2>/dev/null || true)"
rm -rf "$VALIDATION_DIR"
printf '%s' "$DOCTOR_JSON" | grep -Fq '"version"' || \
  die "candidate binary did not return a Session Skein doctor JSON object"
printf '%s' "$DOCTOR_JSON" | grep -Fq "$ACTUAL_VERSION" || \
  die "candidate binary version and doctor output disagree"
for required_field in '"config_dir"' '"data_dir"' '"database"'; do
  printf '%s' "$DOCTOR_JSON" | grep -Fq "$required_field" || \
    die "candidate binary doctor output is missing $required_field"
done

if [ -n "$SOURCE_DIR" ] && [ "$NO_SKILL" -eq 0 ]; then
  SKILL_VERSION="$(sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$PLUGIN_DIR/.codex-plugin/plugin.json" | head -n 1)"
  [ "$SKILL_VERSION" = "$ACTUAL_VERSION" ] || \
    die "binary $ACTUAL_VERSION and bundled skill/plugin $SKILL_VERSION do not match"
fi
[ -z "$RELEASE_VERSION" ] || [ "$ACTUAL_VERSION" = "$RELEASE_VERSION" ] || \
  die "candidate binary reports $ACTUAL_VERSION, expected $RELEASE_VERSION"
if [ "$CHECK_ONLY" -eq 1 ]; then
  if [ "$JSON_OUTPUT" -eq 1 ]; then
    printf '{"channel":"%s","targetVersion":"%s","platform":"%s","verified":true}\n' \
      "$RELEASE_CHANNEL" "$ACTUAL_VERSION" "$(detect_release_target)"
  else
    printf 'verified Session Skein %s for %s\n' "$ACTUAL_VERSION" "$(detect_release_target)"
  fi
  exit 0
fi
INCOMING_HASH="$(sha256_file "$BINARY_SOURCE")"

if [ -n "$PREVIOUS_INSTALLER" ]; then
  [ "$PREVIOUS_INSTALLER" = "$INSTALLER_SNAPSHOT" ] || \
    die "receipt installer path disagrees with $INSTALLER_SNAPSHOT"
  [ -f "$PREVIOUS_INSTALLER" ] && \
    [ "$(sha256_file "$PREVIOUS_INSTALLER")" = "$PREVIOUS_INSTALLER_HASH" ] || \
    die "installer snapshot ownership drift detected: $PREVIOUS_INSTALLER"
fi

# Preflight every collision before changing the binary, state, skill, or MCP config.
[ -z "$PREVIOUS_BINARY" ] || [ "$PREVIOUS_BINARY" = "$INSTALLED_BINARY" ] || \
  die "receipt owns $PREVIOUS_BINARY; uninstall before changing --bin-dir"
BINARY_BACKUP="$PREVIOUS_BINARY_BACKUP"
BINARY_ACTION="install"
if [ -e "$INSTALLED_BINARY" ]; then
  CURRENT_BINARY_HASH="$(sha256_file "$INSTALLED_BINARY")"
  if [ "$PREVIOUS_BINARY" = "$INSTALLED_BINARY" ] && \
     [ -n "$PREVIOUS_BINARY_HASH" ] && [ "$CURRENT_BINARY_HASH" = "$PREVIOUS_BINARY_HASH" ]; then
    if [ "$CURRENT_BINARY_HASH" = "$INCOMING_HASH" ]; then
      BINARY_ACTION="keep-owned"
    else
      BINARY_ACTION="replace-owned"
    fi
  elif [ "$REPLACE_BINARY" -eq 1 ]; then
    BINARY_ACTION="backup-replace"
    BINARY_BACKUP="$INSTALLED_BINARY.backup.$(date -u +%Y%m%d%H%M%S)"
  else
    die "destination binary is not installer-owned: $INSTALLED_BINARY (use --replace-binary)"
  fi
fi

SKILL_TARGET="$PREVIOUS_SKILL"
SKILL_SOURCE="$PREVIOUS_SKILL_SOURCE"
SKILL_BACKUP="$PREVIOUS_SKILL_BACKUP"
SKILL_ACTION="none"
if [ "$NO_SKILL" -eq 0 ]; then
  DESIRED_SKILL_ORIGIN="$PLUGIN_DIR/skills/session-skein"
  DESIRED_SKILL_HASH="$(sha256_tree "$DESIRED_SKILL_ORIGIN")"
  DESIRED_SKILL_SOURCE="$SKILL_SNAPSHOT_ROOT/$ACTUAL_VERSION-$DESIRED_SKILL_HASH"
  DESIRED_SKILL_TARGET="$CODEX_HOME_DIR/skills/session-skein"
  [ -z "$PREVIOUS_SKILL" ] || [ "$PREVIOUS_SKILL" = "$DESIRED_SKILL_TARGET" ] || \
    die "receipt owns $PREVIOUS_SKILL; uninstall before changing CODEX_HOME"
  SKILL_TARGET="$DESIRED_SKILL_TARGET"
  SKILL_SOURCE="$DESIRED_SKILL_SOURCE"
  if [ ! -e "$DESIRED_SKILL_TARGET" ] && [ ! -L "$DESIRED_SKILL_TARGET" ]; then
    SKILL_ACTION="create"
  elif [ -L "$DESIRED_SKILL_TARGET" ] && \
       [ "$(readlink "$DESIRED_SKILL_TARGET" || true)" = "$DESIRED_SKILL_SOURCE" ]; then
    if [ "$PREVIOUS_SKILL" != "$DESIRED_SKILL_TARGET" ] || \
       [ "$PREVIOUS_SKILL_SOURCE" != "$DESIRED_SKILL_SOURCE" ]; then
      # Respect a pre-existing matching link without claiming it for uninstall.
      SKILL_TARGET=""
      SKILL_SOURCE=""
    fi
  elif [ -n "$PREVIOUS_SKILL" ] && \
       [ "$PREVIOUS_SKILL" = "$DESIRED_SKILL_TARGET" ] && \
       [ -L "$DESIRED_SKILL_TARGET" ] && \
       [ "$(readlink "$DESIRED_SKILL_TARGET" || true)" = "$PREVIOUS_SKILL_SOURCE" ]; then
    SKILL_ACTION="replace-owned"
  elif [ "$REPLACE_SKILL" -eq 1 ]; then
    SKILL_ACTION="backup-create"
    SKILL_BACKUP="$DESIRED_SKILL_TARGET.backup.$(date -u +%Y%m%d%H%M%S)"
  else
    die "skill path is not installer-owned: $DESIRED_SKILL_TARGET (use --replace-skill)"
  fi
fi

MCP_PROFILE="$PREVIOUS_MCP_PROFILE"
MCP_HASH="$PREVIOUS_MCP_HASH"
MCP_SPEC_HASH="$PREVIOUS_MCP_SPEC_HASH"
MCP_ACTION="none"
CURRENT_MCP=""
if [ "$NO_MCP" -eq 0 ]; then
  command -v codex >/dev/null 2>&1 || die "codex CLI is required unless --no-mcp is used"
  DESIRED_MCP_SPEC="command=$INSTALLED_BINARY
profile=$PROFILE
SKEIN_CONFIG_DIR=${SKEIN_CONFIG_DIR:-}
SKEIN_DATA_DIR=${SKEIN_DATA_DIR:-}
SKEIN_CODEX_BIN=${SKEIN_CODEX_BIN:-}
CODEX_HOME=${CODEX_HOME:-}"
  DESIRED_MCP_SPEC_HASH="$(printf '%s' "$DESIRED_MCP_SPEC" | sha256_text)"
  CURRENT_MCP="$(codex_mcp_json)"
  if [ -n "$CURRENT_MCP" ]; then
    CURRENT_MCP_HASH="$(printf '%s' "$CURRENT_MCP" | sha256_text)"
    if [ -n "$PREVIOUS_MCP_HASH" ] && [ "$CURRENT_MCP_HASH" = "$PREVIOUS_MCP_HASH" ]; then
      if [ -n "$PREVIOUS_MCP_SPEC_HASH" ] && \
         [ "$PREVIOUS_MCP_SPEC_HASH" = "$DESIRED_MCP_SPEC_HASH" ]; then
        MCP_ACTION="none"
      else
        MCP_ACTION="replace-owned"
      fi
    elif [ "$REPLACE_MCP" -eq 1 ]; then
      MCP_ACTION="backup-replace"
    else
      die "session-skein MCP registration is not installer-owned (use --replace-mcp after reviewing it)"
    fi
  else
    MCP_ACTION="add"
  fi
fi

mkdir -p "$BIN_DIR" "$INSTALL_STATE_DIR"
chmod 0700 "$INSTALL_STATE_DIR" 2>/dev/null || true
if [ "$NO_SKILL" -eq 0 ]; then
  if [ -e "$DESIRED_SKILL_SOURCE" ]; then
    [ -d "$DESIRED_SKILL_SOURCE" ] || \
      die "skill snapshot path is not a directory: $DESIRED_SKILL_SOURCE"
    [ "$(sha256_tree "$DESIRED_SKILL_SOURCE")" = "$DESIRED_SKILL_HASH" ] || \
      die "skill snapshot content does not match its content address"
  else
    mkdir -p "$SKILL_SNAPSHOT_ROOT"
    STAGED_SKILL="$SKILL_SNAPSHOT_ROOT/.session-skein.install.$$"
    rm -rf "$STAGED_SKILL"
    mkdir -p "$STAGED_SKILL"
    cp -R "$DESIRED_SKILL_ORIGIN/." "$STAGED_SKILL/"
    if [ "$(sha256_tree "$STAGED_SKILL")" != "$DESIRED_SKILL_HASH" ]; then
      rm -rf "$STAGED_SKILL"
      die "copied skill snapshot failed content verification"
    fi
    if ! mv "$STAGED_SKILL" "$DESIRED_SKILL_SOURCE"; then
      rm -rf "$STAGED_SKILL"
      die "could not install the immutable skill snapshot"
    fi
  fi
fi
rm -f "$RECEIPT_ROLLBACK" "$BINARY_ROLLBACK" "$INSTALLER_ROLLBACK" "$MCP_ROLLBACK" "$MCP_JSON_ROLLBACK"
HAD_RECEIPT=0
if [ -f "$RECEIPT" ]; then
  cp "$RECEIPT" "$RECEIPT_ROLLBACK"
  HAD_RECEIPT=1
fi

restore_binary_and_receipt() {
  if [ "$BINARY_ACTION" != "keep-owned" ]; then
    rm -f "$INSTALLED_BINARY"
    if [ "$BINARY_ACTION" = "replace-owned" ] && [ -e "$BINARY_ROLLBACK" ]; then
      mv "$BINARY_ROLLBACK" "$INSTALLED_BINARY"
    elif [ "$BINARY_ACTION" = "backup-replace" ] && [ -e "$BINARY_BACKUP" ]; then
      mv "$BINARY_BACKUP" "$INSTALLED_BINARY"
    fi
  fi
  if [ -n "${INCOMING_INSTALLER:-}" ]; then
    rm -f "$INSTALLER_SNAPSHOT"
    if [ -n "$PREVIOUS_INSTALLER" ] && [ -f "$INSTALLER_ROLLBACK" ]; then
      mv -f "$INSTALLER_ROLLBACK" "$INSTALLER_SNAPSHOT"
    fi
  fi
  if [ "$HAD_RECEIPT" -eq 1 ] && [ -f "$RECEIPT_ROLLBACK" ]; then
    mv -f "$RECEIPT_ROLLBACK" "$RECEIPT"
  else
    rm -f "$RECEIPT"
  fi
}

INSTALLER_OWNED="$PREVIOUS_INSTALLER"
INSTALLER_OWNED_HASH="$PREVIOUS_INSTALLER_HASH"
INCOMING_INSTALLER=""
if [ -f "$SOURCE_DIR/install.sh" ]; then
  INCOMING_INSTALLER="$INSTALL_STATE_DIR/.installer.$$"
  cp "$SOURCE_DIR/install.sh" "$INCOMING_INSTALLER"
  chmod 0755 "$INCOMING_INSTALLER"
  bash -n "$INCOMING_INSTALLER" || die "incoming installer snapshot failed syntax validation"
  INCOMING_INSTALLER_HASH="$(sha256_file "$INCOMING_INSTALLER")"
  if [ -n "$PREVIOUS_INSTALLER" ]; then
    cp "$PREVIOUS_INSTALLER" "$INSTALLER_ROLLBACK"
  fi
  if [ "${SKEIN_TEST_FAIL_INSTALLER_SNAPSHOT:-0}" = "1" ] && \
     [ "${SKEIN_ALLOW_RELEASE_OVERRIDE:-0}" = "1" ]; then
    rm -f "$INCOMING_INSTALLER"
    restore_binary_and_receipt
    die "injected installer snapshot installation failure"
  fi
  if ! mv -f "$INCOMING_INSTALLER" "$INSTALLER_SNAPSHOT"; then
    restore_binary_and_receipt
    die "could not install the verified installer snapshot"
  fi
  INSTALLER_OWNED="$INSTALLER_SNAPSHOT"
  INSTALLER_OWNED_HASH="$INCOMING_INSTALLER_HASH"
fi

if [ "$BINARY_ACTION" = "keep-owned" ]; then
  INSTALLED_HASH="$CURRENT_BINARY_HASH"
  note "$INSTALLED_BINARY is already the requested build"
else
  STAGED_BINARY="$BIN_DIR/.skein.install.$$"
  cp "$BINARY_SOURCE" "$STAGED_BINARY"
  chmod 0755 "$STAGED_BINARY"
  "$STAGED_BINARY" --version >/dev/null
  if [ "$BINARY_ACTION" = "replace-owned" ]; then
    cp "$INSTALLED_BINARY" "$BINARY_ROLLBACK"
  elif [ "$BINARY_ACTION" = "backup-replace" ]; then
    mv "$INSTALLED_BINARY" "$BINARY_BACKUP"
    note "Backed up existing binary to $BINARY_BACKUP"
  fi
  if ! mv -f "$STAGED_BINARY" "$INSTALLED_BINARY"; then
    restore_binary_and_receipt
    die "could not replace $INSTALLED_BINARY"
  fi
  INSTALLED_HASH="$(sha256_file "$INSTALLED_BINARY")"
  note "Installed $INSTALLED_BINARY"
fi

write_receipt() {
  cat > "$RECEIPT" <<EOF
version=$ACTUAL_VERSION
binary=$INSTALLED_BINARY
binary_hash=$INSTALLED_HASH
binary_backup=$BINARY_BACKUP
source=${SOURCE_RECEIPT:-$SOURCE_DIR}
source_commit=$SOURCE_COMMIT
installer=$INSTALLER_OWNED
installer_hash=$INSTALLER_OWNED_HASH
skill=$SKILL_TARGET
skill_source=$SKILL_SOURCE
skill_backup=$SKILL_BACKUP
mcp_profile=$MCP_PROFILE
mcp_hash=$MCP_HASH
mcp_spec_hash=$MCP_SPEC_HASH
EOF
}

# Write provisional ownership immediately; later failures remain uninstallable.
if ! write_receipt; then
  restore_binary_and_receipt
  die "could not write the provisional installer receipt; the previous binary was restored"
fi
if [ "${SKEIN_TEST_FAIL_INSTALLER_RECEIPT:-0}" = "1" ] && \
   [ "${SKEIN_ALLOW_RELEASE_OVERRIDE:-0}" = "1" ]; then
  restore_binary_and_receipt
  die "injected installer snapshot receipt failure; the previous installation was restored"
fi
if ! "$INSTALLED_BINARY" init >/dev/null; then
  restore_binary_and_receipt
  die "Session Skein initialization failed; the previous binary and receipt were restored"
fi

fail_skill_install() {
  restore_binary_and_receipt
  die "$*; the previous binary and receipt were restored"
}

rollback_skill_switch() {
  case "$SKILL_ACTION" in
    create)
      rm -f "$DESIRED_SKILL_TARGET"
      ;;
    replace-owned)
      rm -f "$DESIRED_SKILL_TARGET"
      [ -z "${OLD_SKILL_SOURCE:-}" ] || \
        ln -s "$OLD_SKILL_SOURCE" "$DESIRED_SKILL_TARGET" || true
      ;;
    backup-create)
      rm -f "$DESIRED_SKILL_TARGET"
      if [ -e "$SKILL_BACKUP" ] || [ -L "$SKILL_BACKUP" ]; then
        mv "$SKILL_BACKUP" "$DESIRED_SKILL_TARGET" || true
      fi
      ;;
  esac
}

if [ "$SKILL_ACTION" = "backup-create" ]; then
  mkdir -p "$(dirname -- "$DESIRED_SKILL_TARGET")" || \
    fail_skill_install "could not create the Codex skills directory"
  if ! mv "$DESIRED_SKILL_TARGET" "$SKILL_BACKUP"; then
    fail_skill_install "could not back up the existing skill path"
  fi
  note "Backed up existing skill to $SKILL_BACKUP"
  if ! ln -s "$DESIRED_SKILL_SOURCE" "$DESIRED_SKILL_TARGET"; then
    mv "$SKILL_BACKUP" "$DESIRED_SKILL_TARGET"
    fail_skill_install "could not create the Codex skill link"
  fi
elif [ "$SKILL_ACTION" = "replace-owned" ]; then
  OLD_SKILL_SOURCE="$(readlink "$DESIRED_SKILL_TARGET")"
  if ! rm -f "$DESIRED_SKILL_TARGET"; then
    fail_skill_install "could not detach the previous Codex skill snapshot"
  fi
  if ! ln -s "$DESIRED_SKILL_SOURCE" "$DESIRED_SKILL_TARGET"; then
    ln -s "$OLD_SKILL_SOURCE" "$DESIRED_SKILL_TARGET" || true
    fail_skill_install "could not switch the Codex skill snapshot"
  fi
elif [ "$SKILL_ACTION" = "create" ]; then
  mkdir -p "$(dirname -- "$DESIRED_SKILL_TARGET")" || \
    fail_skill_install "could not create the Codex skills directory"
  if ! ln -s "$DESIRED_SKILL_SOURCE" "$DESIRED_SKILL_TARGET"; then
    fail_skill_install "could not create the Codex skill link"
  fi
fi
if [ "$SKILL_ACTION" != "none" ]; then
  note "Installed Codex skill $DESIRED_SKILL_TARGET"
  if ! write_receipt; then
    rollback_skill_switch
    fail_skill_install "could not record the installed skill"
  fi
fi
if [ "$MCP_ACTION" != "none" ]; then
  MCP_CONFIG_EXISTED=0
  if [ -f "$CODEX_CONFIG_FILE" ]; then
    cp "$CODEX_CONFIG_FILE" "$MCP_ROLLBACK"
    MCP_CONFIG_EXISTED=1
  fi
  if [ -n "$CURRENT_MCP" ]; then
    printf '%s\n' "$CURRENT_MCP" > "$MCP_JSON_ROLLBACK"
  fi
  if [ "$MCP_ACTION" = "backup-replace" ]; then
    printf '%s\n' "$CURRENT_MCP" > "$MCP_BACKUP"
    note "Backed up the previous MCP JSON to $MCP_BACKUP"
  fi

  restore_codex_config() {
    if [ "$MCP_CONFIG_EXISTED" -eq 1 ] && [ -f "$MCP_ROLLBACK" ]; then
      mkdir -p "$(dirname -- "$CODEX_CONFIG_FILE")"
      cp "$MCP_ROLLBACK" "$CODEX_CONFIG_FILE"
    else
      rm -f "$CODEX_CONFIG_FILE"
    fi
  }

  MCP_ARGS=(mcp add session-skein)
  [ -z "${SKEIN_CONFIG_DIR:-}" ] || MCP_ARGS+=(--env "SKEIN_CONFIG_DIR=$SKEIN_CONFIG_DIR")
  [ -z "${SKEIN_DATA_DIR:-}" ] || MCP_ARGS+=(--env "SKEIN_DATA_DIR=$SKEIN_DATA_DIR")
  [ -z "${SKEIN_CODEX_BIN:-}" ] || MCP_ARGS+=(--env "SKEIN_CODEX_BIN=$SKEIN_CODEX_BIN")
  [ -z "${CODEX_HOME:-}" ] || MCP_ARGS+=(--env "CODEX_HOME=$CODEX_HOME")
  MCP_ARGS+=(-- "$INSTALLED_BINARY" mcp)
  [ "$PROFILE" = "catalog" ] || MCP_ARGS+=(--allow-control)
  if ! NO_COLOR=1 FORCE_COLOR=0 codex "${MCP_ARGS[@]}" >/dev/null; then
    restore_codex_config
    rollback_skill_switch
    restore_binary_and_receipt
    die "Codex MCP registration failed; the previous owned integration was restored"
  fi
  CONFIGURED_MCP="$(codex_mcp_json)"
  if [ -z "$CONFIGURED_MCP" ]; then
    restore_codex_config
    rollback_skill_switch
    restore_binary_and_receipt
    die "Codex could not verify the MCP registration; the previous owned integration was restored"
  fi
  MCP_PROFILE="$PROFILE"
  MCP_HASH="$(printf '%s' "$CONFIGURED_MCP" | sha256_text)"
  MCP_SPEC_HASH="$DESIRED_MCP_SPEC_HASH"
  if ! write_receipt; then
    restore_codex_config
    rollback_skill_switch
    restore_binary_and_receipt
    die "could not record the MCP registration; the previous owned integration was restored"
  fi
  rm -f "$MCP_ROLLBACK" "$MCP_JSON_ROLLBACK"
  note "Registered the $PROFILE Session Skein MCP profile"
fi

rm -f "$BINARY_ROLLBACK" "$RECEIPT_ROLLBACK" "$INSTALLER_ROLLBACK" "$MCP_ROLLBACK" "$MCP_JSON_ROLLBACK"

printf '\n✓ Session Skein is installed.\n'
"$INSTALLED_BINARY" --version
"$INSTALLED_BINARY" doctor
if [ "$NO_MCP" -eq 0 ]; then
  codex_mcp_json
fi
printf '\nStart a new Codex session so it discovers the skill and MCP server.\n'
printf 'No scan root, private context source, daemon, or worker was enabled.\n'
case ":${PATH:-}:" in
  *":$BIN_DIR:"*) ;;
  *) printf 'Add %s to PATH to invoke skein by name in future shells.\n' "$BIN_DIR" ;;
esac
