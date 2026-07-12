#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
BINARY="${1:-$REPO_ROOT/target/debug/skein}"
BINARY="$(CDPATH= cd -- "$(dirname -- "$BINARY")" && pwd -P)/$(basename -- "$BINARY")"
FAKE_BIN="$REPO_ROOT/tests/fixtures/fake-codex"
ORIGINAL_PATH="$PATH"
TRUE_BIN="/usr/bin/true"
[ -x "$TRUE_BIN" ] || TRUE_BIN="/bin/true"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

run_install() {
  case_root="$1"
  shift
  env \
    HOME="$case_root/home" \
    CODEX_HOME="$case_root/codex" \
    XDG_STATE_HOME="$case_root/state" \
    SKEIN_DATA_DIR="$case_root/data" \
    SKEIN_CONFIG_DIR="$case_root/config" \
    FAKE_CODEX_STATE="$case_root/codex/config.toml" \
    PATH="$FAKE_BIN:$ORIGINAL_PATH" \
    "$@"
}

# Clean control install, repeat with integration changes disabled, then uninstall.
clean="$ROOT/clean"
run_install "$clean" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$clean/bin" --control >/dev/null
run_install "$clean" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$clean/bin" --no-skill --no-mcp >/dev/null
run_install "$clean" "$REPO_ROOT/install.sh" --uninstall >/dev/null
test ! -e "$clean/bin/skein"
test ! -e "$clean/codex/skills/session-skein"
test ! -e "$clean/codex/config.toml"

# An unowned binary is refused before state/skill mutation.
binary_collision="$ROOT/binary-collision"
mkdir -p "$binary_collision/bin"
cp "$TRUE_BIN" "$binary_collision/bin/skein"
if run_install "$binary_collision" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$binary_collision/bin" --no-mcp >/dev/null 2>&1; then
  printf 'unowned binary collision unexpectedly succeeded\n' >&2
  exit 1
fi
test ! -e "$binary_collision/codex/skills/session-skein"
test ! -e "$binary_collision/state/session-skein/install/receipt"

# Explicit replacement is backed up and restored by uninstall.
original_hash="$(hash_file "$binary_collision/bin/skein")"
run_install "$binary_collision" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$binary_collision/bin" --no-mcp --replace-binary >/dev/null
run_install "$binary_collision" "$REPO_ROOT/install.sh" --uninstall >/dev/null
test "$(hash_file "$binary_collision/bin/skein")" = "$original_hash"

# A generic successful executable is not accepted as Session Skein.
identity="$ROOT/identity"
if run_install "$identity" "$REPO_ROOT/install.sh" --binary "$TRUE_BIN" \
  --bin-dir "$identity/bin" --no-skill --no-mcp >/dev/null 2>&1; then
  printf 'non-Skein binary unexpectedly succeeded\n' >&2
  exit 1
fi
test ! -e "$identity/state/session-skein/install/receipt"

# Skill collision is detected before the binary or database changes.
skill_collision="$ROOT/skill-collision"
mkdir -p "$skill_collision/codex/skills/session-skein"
touch "$skill_collision/codex/skills/session-skein/user-owned"
if run_install "$skill_collision" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$skill_collision/bin" --no-mcp >/dev/null 2>&1; then
  printf 'unowned skill collision unexpectedly succeeded\n' >&2
  exit 1
fi
test ! -e "$skill_collision/bin/skein"
test -e "$skill_collision/codex/skills/session-skein/user-owned"

# MCP collision is detected before binary/state mutation; explicit replacement is
# backed up, installed, and ownership-safe on uninstall.
mcp_collision="$ROOT/mcp-collision"
mkdir -p "$mcp_collision/codex"
printf '%s\n' '{"name":"session-skein","transport":{"command":"other","args":[]}}' \
  > "$mcp_collision/codex/config.toml"
if run_install "$mcp_collision" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$mcp_collision/bin" --no-skill >/dev/null 2>&1; then
  printf 'unowned MCP collision unexpectedly succeeded\n' >&2
  exit 1
fi
test ! -e "$mcp_collision/bin/skein"
test ! -e "$mcp_collision/state/session-skein/install/receipt"
run_install "$mcp_collision" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$mcp_collision/bin" --no-skill --replace-mcp --control >/dev/null
test -e "$mcp_collision/state/session-skein/install/replaced-mcp.json"
run_install "$mcp_collision" "$REPO_ROOT/install.sh" --uninstall >/dev/null
test ! -e "$mcp_collision/bin/skein"
test ! -e "$mcp_collision/codex/config.toml"

# A fresh initialization failure rolls back its binary and provisional receipt.
partial="$ROOT/partial"
if env \
  HOME="$partial/home" CODEX_HOME="$partial/codex" \
  XDG_STATE_HOME="$partial/state" SKEIN_DATA_DIR="/proc/session-skein-unwritable" \
  SKEIN_CONFIG_DIR="$partial/config" PATH="$ORIGINAL_PATH" \
  "$REPO_ROOT/install.sh" --binary "$BINARY" --bin-dir "$partial/bin" \
  --no-skill --no-mcp >/dev/null 2>&1; then
  printf 'forced init failure unexpectedly succeeded\n' >&2
  exit 1
fi
test ! -e "$partial/bin/skein"
test ! -e "$partial/state/session-skein/install/receipt"

# An owned update retains the previous executable and receipt until real-state
# initialization succeeds.
rollback="$ROOT/rollback"
mkdir -p "$rollback/candidate"
cp "$BINARY" "$rollback/candidate/old-skein"
printf 'previous-build' >> "$rollback/candidate/old-skein"
chmod 0755 "$rollback/candidate/old-skein"
run_install "$rollback" "$REPO_ROOT/install.sh" \
  --binary "$rollback/candidate/old-skein" --bin-dir "$rollback/bin" \
  --no-skill --no-mcp >/dev/null
old_installed_hash="$(hash_file "$rollback/bin/skein")"
old_receipt_hash="$(hash_file "$rollback/state/session-skein/install/receipt")"
if run_install "$rollback" env SKEIN_DATA_DIR=/proc/session-skein-unwritable \
  "$REPO_ROOT/install.sh" --binary "$BINARY" --bin-dir "$rollback/bin" \
  --no-skill --no-mcp >/dev/null 2>&1; then
  printf 'owned update with forced init failure unexpectedly succeeded\n' >&2
  exit 1
fi
test "$(hash_file "$rollback/bin/skein")" = "$old_installed_hash"
test "$(hash_file "$rollback/state/session-skein/install/receipt")" = "$old_receipt_hash"

# Reinstalling against another Codex home is refused before the existing skill is
# orphaned.
changed_home="$ROOT/changed-home"
run_install "$changed_home" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$changed_home/bin" --no-mcp >/dev/null
if run_install "$changed_home" env CODEX_HOME="$changed_home/other-codex" \
  "$REPO_ROOT/install.sh" --binary "$BINARY" --bin-dir "$changed_home/bin" \
  --no-mcp >/dev/null 2>&1; then
  printf 'CODEX_HOME change unexpectedly succeeded\n' >&2
  exit 1
fi
test -L "$changed_home/codex/skills/session-skein"
test ! -e "$changed_home/other-codex/skills/session-skein"

# Missing installed replacements still restore their user-owned backups.
missing="$ROOT/missing-replacements"
mkdir -p "$missing/bin" "$missing/codex/skills/session-skein"
cp "$TRUE_BIN" "$missing/bin/skein"
touch "$missing/codex/skills/session-skein/user-owned"
missing_binary_hash="$(hash_file "$missing/bin/skein")"
run_install "$missing" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$missing/bin" --no-mcp --replace-binary --replace-skill >/dev/null
rm -f "$missing/bin/skein" "$missing/codex/skills/session-skein"
run_install "$missing" "$REPO_ROOT/install.sh" --uninstall >/dev/null
test "$(hash_file "$missing/bin/skein")" = "$missing_binary_hash"
test -e "$missing/codex/skills/session-skein/user-owned"

# MCP writes are rolled back if Codex fails after changing its configuration or
# cannot verify the newly written entry.
mcp_failure="$ROOT/mcp-failure"
mkdir -p "$mcp_failure/codex"
printf '%s\n' '{"name":"session-skein","transport":{"command":"original","args":[]}}' \
  > "$mcp_failure/codex/config.toml"
original_mcp_hash="$(hash_file "$mcp_failure/codex/config.toml")"
if run_install "$mcp_failure" env FAKE_CODEX_FAIL_ADD=after \
  "$REPO_ROOT/install.sh" --binary "$BINARY" --bin-dir "$mcp_failure/bin" \
  --no-skill --replace-mcp >/dev/null 2>&1; then
  printf 'MCP add failure unexpectedly succeeded\n' >&2
  exit 1
fi
test "$(hash_file "$mcp_failure/codex/config.toml")" = "$original_mcp_hash"

mcp_verify_failure="$ROOT/mcp-verify-failure"
if run_install "$mcp_verify_failure" env FAKE_CODEX_FAIL_VERIFY=1 \
  "$REPO_ROOT/install.sh" --binary "$BINARY" --bin-dir "$mcp_verify_failure/bin" \
  --no-skill >/dev/null 2>&1; then
  printf 'MCP verification failure unexpectedly succeeded\n' >&2
  exit 1
fi
test ! -e "$mcp_verify_failure/codex/config.toml"

# Uninstall retains MCP ownership when Codex cannot answer authoritatively.
unqueryable="$ROOT/unqueryable-mcp"
run_install "$unqueryable" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$unqueryable/bin" --no-skill >/dev/null
run_install "$unqueryable" env FAKE_CODEX_FAIL_GET=1 \
  "$REPO_ROOT/install.sh" --uninstall >/dev/null 2>&1
test -e "$unqueryable/codex/config.toml"
test -e "$unqueryable/state/session-skein/install/receipt"

# A real Git update that fails in the refreshed installer cannot change the live
# content-addressed skill snapshot.
update_case="$ROOT/update-failure"
update_fixture="$ROOT/update-fixture"
mkdir -p "$update_fixture/work/plugins/session-skein" "$update_fixture/remote.git"
git init --bare "$update_fixture/remote.git" >/dev/null
git init -b main "$update_fixture/work" >/dev/null
cp "$REPO_ROOT/install.sh" "$REPO_ROOT/Cargo.toml" "$update_fixture/work/"
cp -R "$REPO_ROOT/plugins/session-skein/.codex-plugin" \
  "$REPO_ROOT/plugins/session-skein/skills" "$update_fixture/work/plugins/session-skein/"
git -C "$update_fixture/work" add .
git -C "$update_fixture/work" -c user.name='Installer Test' \
  -c user.email='installer@example.invalid' commit -m initial >/dev/null
git -C "$update_fixture/work" remote add origin "$update_fixture/remote.git"
git -C "$update_fixture/work" push -u origin main >/dev/null
git --git-dir="$update_fixture/remote.git" symbolic-ref HEAD refs/heads/main
git clone "$update_fixture/remote.git" "$update_fixture/managed" >/dev/null
run_install "$update_case" "$update_fixture/managed/install.sh" --binary "$BINARY" \
  --bin-dir "$update_case/bin" --no-mcp >/dev/null
update_receipt_hash="$(hash_file "$update_case/state/session-skein/install/receipt")"
update_skill_target="$(readlink "$update_case/codex/skills/session-skein")"
printf '\nupdate-marker-must-not-go-live\n' >> \
  "$update_fixture/work/plugins/session-skein/skills/session-skein/SKILL.md"
printf '%s\n' '#!/usr/bin/env bash' 'exit 42' > "$update_fixture/work/install.sh"
chmod 0755 "$update_fixture/work/install.sh"
git -C "$update_fixture/work" add .
git -C "$update_fixture/work" -c user.name='Installer Test' \
  -c user.email='installer@example.invalid' commit -m failing-update >/dev/null
git -C "$update_fixture/work" push >/dev/null
if run_install "$update_case" "$update_fixture/managed/install.sh" --binary "$BINARY" \
  --bin-dir "$update_case/bin" --no-mcp --update >/dev/null 2>&1; then
  printf 'failing Git update unexpectedly succeeded\n' >&2
  exit 1
fi
test "$(readlink "$update_case/codex/skills/session-skein")" = "$update_skill_target"
test "$(hash_file "$update_case/state/session-skein/install/receipt")" = "$update_receipt_hash"
if grep -Fq 'update-marker-must-not-go-live' \
  "$update_case/codex/skills/session-skein/SKILL.md"; then
  printf 'failed update changed the active skill snapshot\n' >&2
  exit 1
fi
run_install "$update_case" "$REPO_ROOT/install.sh" --uninstall >/dev/null

# User replacement after installation is preserved and keeps the receipt for review.
modified="$ROOT/modified"
run_install "$modified" "$REPO_ROOT/install.sh" --binary "$BINARY" \
  --bin-dir "$modified/bin" --no-skill --no-mcp >/dev/null
cp "$TRUE_BIN" "$modified/bin/skein"
run_install "$modified" "$REPO_ROOT/install.sh" --uninstall >/dev/null
test -e "$modified/bin/skein"
test -e "$modified/state/session-skein/install/receipt"

printf 'Unix installer lifecycle and collision tests passed.\n'
