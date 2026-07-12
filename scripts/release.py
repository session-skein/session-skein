#!/usr/bin/env python3
"""Deterministic Session Skein preview-release packaging and validation."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import shutil
import stat
import tarfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PLUGIN = ROOT / "plugins" / "session-skein"
REQUIRED_TARGETS = {
    "x86_64-unknown-linux-gnu": ".tar.gz",
    "x86_64-apple-darwin": ".tar.gz",
    "aarch64-apple-darwin": ".tar.gz",
    "x86_64-pc-windows-msvc": ".zip",
}
FIXED_ZIP_TIME = (1980, 1, 1, 0, 0, 0)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def workspace_version() -> str:
    for line in (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        if line.startswith("version = "):
            return line.split('"', 2)[1]
    raise SystemExit("workspace package version was not found")


def plugin_version() -> str:
    manifest = json.loads((PLUGIN / ".codex-plugin" / "plugin.json").read_text())
    return str(manifest["version"])


def validate_versions(expected: str | None = None) -> str:
    version = workspace_version()
    if expected is not None and version != expected:
        raise SystemExit(f"workspace version {version} does not match {expected}")
    if plugin_version() != version:
        raise SystemExit("workspace and plugin versions differ")
    lock = (ROOT / "Cargo.lock").read_text(encoding="utf-8")
    if lock.count(f'name = "session-skein"\nversion = "{version}"') != 1:
        raise SystemExit("Cargo.lock session-skein version differs")
    for crate in ("skein-codex", "skein-core"):
        if f'name = "{crate}"\nversion = "{version}"' not in lock:
            raise SystemExit(f"Cargo.lock {crate} version differs")
    return version


def package_files(binary: Path) -> list[tuple[Path, str, int]]:
    executable = "skein.exe" if binary.suffix == ".exe" else "skein"
    files = [
        (binary, executable, 0o755),
        (ROOT / "README.md", "README.md", 0o644),
        (ROOT / "LICENSE", "LICENSE", 0o644),
        (ROOT / "install.sh", "install.sh", 0o755),
        (ROOT / "install.ps1", "install.ps1", 0o644),
    ]
    for path in sorted(PLUGIN.rglob("*")):
        if path.is_file():
            files.append((path, f"plugin/{path.relative_to(PLUGIN).as_posix()}", 0o644))
    return files


def build_manifest(version: str, target: str, files: list[tuple[Path, str, int]]) -> dict:
    return {
        "schemaVersion": 1,
        "name": "session-skein",
        "version": version,
        "target": target,
        "files": [
            {"path": destination, "sha256": sha256(source), "size": source.stat().st_size}
            for source, destination, _ in files
        ],
    }


def write_tar(path: Path, prefix: str, files: list[tuple[Path, str, int]], manifest: bytes) -> None:
    with path.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                for source, destination, mode in files:
                    info = archive.gettarinfo(str(source), f"{prefix}/{destination}")
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = 0
                    info.mode = mode
                    with source.open("rb") as handle:
                        archive.addfile(info, handle)
                info = tarfile.TarInfo(f"{prefix}/release-package.json")
                info.size = len(manifest)
                info.mode = 0o644
                info.mtime = info.uid = info.gid = 0
                archive.addfile(info, io.BytesIO(manifest))


def write_zip(path: Path, prefix: str, files: list[tuple[Path, str, int]], manifest: bytes) -> None:
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for source, destination, mode in files:
            info = zipfile.ZipInfo(f"{prefix}/{destination}", FIXED_ZIP_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (stat.S_IFREG | mode) << 16
            archive.writestr(info, source.read_bytes(), compresslevel=9)
        info = zipfile.ZipInfo(f"{prefix}/release-package.json", FIXED_ZIP_TIME)
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = (stat.S_IFREG | 0o644) << 16
        archive.writestr(info, manifest, compresslevel=9)


def package(binary: Path, target: str, output: Path) -> Path:
    version = validate_versions()
    if target not in REQUIRED_TARGETS:
        raise SystemExit(f"unsupported release target: {target}")
    if not binary.is_file():
        raise SystemExit(f"release binary does not exist: {binary}")
    files = package_files(binary)
    manifest = (json.dumps(build_manifest(version, target, files), sort_keys=True, indent=2) + "\n").encode()
    prefix = f"session-skein-v{version}-{target}"
    archive = output / f"{prefix}{REQUIRED_TARGETS[target]}"
    output.mkdir(parents=True, exist_ok=True)
    if archive.suffix == ".zip":
        write_zip(archive, prefix, files, manifest)
    else:
        write_tar(archive, prefix, files, manifest)
    print(archive)
    return archive


def archive_entries(path: Path) -> tuple[list[str], dict]:
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            names = archive.namelist()
            manifest_name = next(name for name in names if name.endswith("/release-package.json"))
            manifest = json.loads(archive.read(manifest_name))
    else:
        with tarfile.open(path, "r:gz") as archive:
            names = archive.getnames()
            member = next(member for member in archive.getmembers() if member.name.endswith("/release-package.json"))
            handle = archive.extractfile(member)
            if handle is None:
                raise SystemExit(f"missing package manifest in {path}")
            manifest = json.load(handle)
    return names, manifest


def assemble(input_dir: Path, output: Path) -> None:
    version = validate_versions()
    archives = sorted(path for path in input_dir.rglob("session-skein-v*") if path.is_file())
    assets = []
    found_targets = set()
    for archive in archives:
        names, manifest = archive_entries(archive)
        target = manifest.get("target")
        if target not in REQUIRED_TARGETS or manifest.get("version") != version:
            raise SystemExit(f"invalid package identity: {archive}")
        required = {"README.md", "LICENSE", "install.sh", "install.ps1", "release-package.json", "plugin/.codex-plugin/plugin.json", "plugin/.mcp.json", "plugin/skills/session-skein/SKILL.md"}
        relative = {name.split("/", 1)[1] for name in names if "/" in name}
        missing = required - relative
        if missing:
            raise SystemExit(f"{archive} is missing: {', '.join(sorted(missing))}")
        if target in found_targets:
            raise SystemExit(f"duplicate package target: {target}")
        found_targets.add(target)
        destination = output / archive.name
        output.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(archive, destination)
        assets.append({"name": destination.name, "target": target, "sha256": sha256(destination), "size": destination.stat().st_size})
    if found_targets != set(REQUIRED_TARGETS):
        raise SystemExit(f"release targets differ: found {sorted(found_targets)}")
    manifest = {"schemaVersion": 1, "name": "session-skein", "version": version, "tag": f"v{version}", "assets": assets}
    manifest_path = output / "release-manifest.json"
    manifest_path.write_text(json.dumps(manifest, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    checksum_assets = [*assets, {"name": manifest_path.name, "sha256": sha256(manifest_path)}]
    (output / "SHA256SUMS").write_text("".join(f"{item['sha256']}  {item['name']}\n" for item in checksum_assets), encoding="utf-8", newline="\n")


def check_ref(ref: str, event: str, output: Path | None) -> None:
    version = validate_versions()
    publish = event == "push" and ref.startswith("refs/tags/")
    if publish and ref != f"refs/tags/v{version}":
        raise SystemExit(f"tag {ref.removeprefix('refs/tags/')} must equal v{version}")
    if output:
        with output.open("a", encoding="utf-8") as handle:
            handle.write(f"version={version}\npublish={'true' if publish else 'false'}\n")
    print(json.dumps({"version": version, "publish": publish}))


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("check-ref")
    check.add_argument("--ref", required=True)
    check.add_argument("--event", required=True)
    check.add_argument("--github-output", type=Path)
    pack = sub.add_parser("package")
    pack.add_argument("--binary", type=Path, required=True)
    pack.add_argument("--target", required=True)
    pack.add_argument("--output", type=Path, required=True)
    assembly = sub.add_parser("assemble")
    assembly.add_argument("--input", type=Path, required=True)
    assembly.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "check-ref":
        check_ref(args.ref, args.event, args.github_output)
    elif args.command == "package":
        package(args.binary, args.target, args.output)
    else:
        assemble(args.input, args.output)


if __name__ == "__main__":
    main()
