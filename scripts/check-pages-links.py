#!/usr/bin/env python3
"""Check internal links in a rendered Session Skein Pages tree."""

from __future__ import annotations

import argparse
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit


class Links(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.links: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag != "a":
            return
        for key, value in attrs:
            if key == "href" and value:
                self.links.append(value)


def target_for(site: Path, source: Path, href: str, baseurl: str) -> Path | None:
    parsed = urlsplit(href)
    if parsed.scheme or parsed.netloc or href.startswith(("mailto:", "tel:")):
        return None
    path = unquote(parsed.path)
    if not path:
        return source
    if path.startswith(baseurl + "/"):
        path = path[len(baseurl) + 1 :]
        target = site / path
    elif path == baseurl:
        target = site
    elif path.startswith("/"):
        target = site / path.lstrip("/")
    else:
        target = source.parent / path
    if target.is_dir():
        target = target / "index.html"
    return target


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("site", type=Path)
    parser.add_argument("--baseurl", default="/session-skein")
    args = parser.parse_args()
    site = args.site.resolve()
    failures: list[str] = []
    for source in sorted(site.rglob("*.html")):
        links = Links()
        links.feed(source.read_text(encoding="utf-8"))
        for href in links.links:
            target = target_for(site, source, href, args.baseurl)
            if target is not None and not target.exists():
                failures.append(f"{source.relative_to(site)} -> {href}")
    if failures:
        raise SystemExit("broken rendered links:\n" + "\n".join(failures))
    print(f"checked {sum(1 for _ in site.rglob('*.html'))} rendered HTML files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
