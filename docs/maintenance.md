# Maintenance

This page covers updates, backups, restore, reset, plugin refresh, and uninstall.

## Update

There is no `skein update` command yet. To reinstall or move to the approved preview,
rerun the normal binary-first installer; unchanged installer-owned objects are
replaced through the receipt rollback path.

The legacy `--update` / `-Update` flags are only for explicit source checkouts. From
a managed or fresh checkout on Linux/macOS:

```console
./install.sh --update --control
```

On Windows:

```powershell
./install.ps1 -Update -Control
```

The installer stages and verifies the new executable before replacing the
installer-owned path, copies a content-addressed skill snapshot, and preserves the
database. The active skill link switches only after `init` applies its transactional
forward migration successfully. A failed refreshed installer cannot change the live
skill through the mutable Git checkout.

If installed manually:

```console
git pull --ff-only
cargo build --workspace --release --locked
install -m 0755 target/release/skein "$HOME/.local/bin/skein"
skein init
```

Back up before an upgrade when you may need to return to an older binary. A schema
migrated by a newer release is not guaranteed to be readable by an older release.

## Plugin refresh

```console
codex plugin marketplace upgrade session-skein
codex plugin add session-skein@session-skein
```

There is no separate plugin-upgrade command in the current Codex CLI. Reinstall from
the refreshed marketplace and start a new Codex thread so cached skills/tools reload.

## Backup and restore

First make sure no foreground controller or reconnectable worker is active:

```console
skein worker list --active
skein control list
```

Use `skein doctor --format json` to locate the data directory. The safest simple
backup is made while Skein and its workers are stopped, then copies the entire data
directory including any `-wal` and `-shm` files.

For a consistent live SQLite backup, use the SQLite CLI's backup command against the
reported database:

```console
sqlite3 /path/from/doctor/skein.sqlite3 ".backup '/safe/path/skein-backup.sqlite3'"
```

Do not publish the backup. It contains private project paths, session metadata,
audit state, indexed identity text, and any explicitly enabled context snippets.

To restore, stop all workers, move the current data directory aside, restore the
complete backup with owner-private permissions, then run:

```console
skein doctor
skein init
```

`init` may migrate a restored older schema.

## Remove a project or root

Scan roots can be removed without the mount being online:

```console
skein scan-root remove /stored/root/path
```

The current CLI intentionally retains projects already discovered from that root.
There is no public project-delete command yet; preserving project/session/audit
relationships is safer than implicit cascading deletion.

## Uninstall the executable and Codex integration

Linux/macOS:

```console
./install.sh --uninstall
```

Windows:

```powershell
./install.ps1 -Uninstall
```

The installer removes only a binary whose current hash matches its receipt, a skill
link whose current target matches, and an MCP registration whose complete JSON hash
is unchanged. Explicitly replaced binary/skill backups are restored. Modified objects
are preserved and the receipt remains for review. The source checkout and private
data are always preserved by default.

If you installed the plugin separately:

```console
codex plugin remove session-skein@session-skein
codex plugin marketplace remove session-skein
```

Remove the marketplace only when it is the Session Skein source and no other retained
plugin depends on it.

## Reset and data purge

Reset is deliberately manual because it destroys project, session, recall, and audit
history.

1. Run `skein worker list --active` and stop or finish every active worker.
2. Run `skein doctor --format json` and record the resolved data directory.
3. Back it up if any history may be useful.
4. Remove that exact data directory using the operating system's normal file tools.
5. Run `skein init` to create empty state.

Never delete `~/.codex` as part of a Session Skein reset; it belongs to Codex and may
contain authentication, configuration, sessions, plugins, and memories.

## Source checkout

Source-mode installation may keep a managed checkout so updates remain reviewable. The active
skill link points to a content-addressed private snapshot, not into the checkout, so
removing the checkout does not immediately break skill discovery. Keep the checkout
when you want `--update`; otherwise a future install can clone it again.
