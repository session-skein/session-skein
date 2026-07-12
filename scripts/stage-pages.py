#!/usr/bin/env python3
"""Stage canonical repository Markdown as a Jekyll Pages source tree."""

from __future__ import annotations

import argparse
import json
import posixpath
import re
import shutil
from pathlib import Path


ROOT_PAGES = (
    "README.md",
    "INSTALL.md",
    "SECURITY.md",
    "CONTRIBUTING.md",
    "ROADMAP.md",
    "CHANGELOG.md",
)


def title_for(path: Path, content: str) -> str:
    for line in content.splitlines():
        if line.startswith("# "):
            return line[2:].strip()
    return path.stem.replace("-", " ").title()


def site_safe_links(content: str, relative: Path, published: set[str], root: Path) -> str:
    def replace(match: re.Match[str]) -> str:
        raw = match.group(1)
        if raw.startswith(("#", "http://", "https://", "mailto:")):
            return match.group(0)
        path, separator, fragment = raw.partition("#")
        normalized = posixpath.normpath((relative.parent / path).as_posix())
        if normalized in published or not (root / normalized).exists():
            return match.group(0)
        suffix = f"#{fragment}" if separator else ""
        return f"](https://github.com/session-skein/session-skein/blob/main/{normalized}{suffix})"

    return re.sub(r"]\(([^)]+)\)", replace, content)


def stage_markdown(
    source: Path, destination: Path, relative: Path, published: set[str], root: Path
) -> None:
    content = source.read_text(encoding="utf-8")
    content = site_safe_links(content, relative, published, root)
    front_matter = (
        "---\n"
        "layout: default\n"
        f"title: {json.dumps(title_for(relative, content))}\n"
        f"canonical_source: {relative.as_posix()}\n"
        "---\n\n"
    )
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(front_matter + content, encoding="utf-8", newline="\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=Path(".pages-source"))
    args = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    output = args.output.resolve()
    if output == root or root not in output.parents:
        parser.error("--output must be a child of the repository root")
    shutil.rmtree(output, ignore_errors=True)
    output.mkdir(parents=True)

    site = root / "site"
    for source in sorted(site.rglob("*")):
        if source.is_file():
            relative = source.relative_to(site)
            target = output / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)

    doc_sources = sorted((root / "docs").rglob("*.md"))
    published = set(ROOT_PAGES)
    published.update(source.relative_to(root).as_posix() for source in doc_sources)

    for name in ROOT_PAGES:
        source = root / name
        if not source.is_file():
            raise SystemExit(f"missing canonical page: {name}")
        stage_markdown(source, output / name, Path(name), published, root)

    for source in doc_sources:
        relative = source.relative_to(root)
        stage_markdown(source, output / relative, relative, published, root)

    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
