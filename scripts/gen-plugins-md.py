#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate docs/plugins.md from the site's per-plugin content files.

The single source for the plugin reference is site/content/docs/plugins/:
each plugin's frontmatter carries its property table, and _index.md carries
the shared prose (conventions, pitfalls) and the category definitions. This
script assembles the repo-facing docs/plugins.md from them, so the site and
the repo doc cannot drift.

Usage:
    python3 scripts/gen-plugins-md.py            # rewrite docs/plugins.md
    python3 scripts/gen-plugins-md.py --check    # exit 1 if out of sync
"""

import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PLUGINS_DIR = ROOT / "site" / "content" / "docs" / "plugins"
OUT = ROOT / "docs" / "plugins.md"

# Site-internal links, mapped back to repo-relative form.
LINK_MAP = [
    ("@/docs/configuration.md", "config.md"),
    ("@/docs/plugins/_index.md", "plugins.md"),
    ("@/docs/protocol.md", "protocol.md"),
    ("@/docs/plugin-api.md", "plugin-api.md"),
    ("@/docs/writing-plugins.md", "plugin-api.md"),
    ("@/docs/ai-authoring.md", "plugin-authoring-claude.md"),
    ("https://github.com/sylflo/veiland/blob/master/docs/examples/", "examples/"),
    ("https://github.com/sylflo/veiland/tree/master/docs/examples", "examples/"),
]

HEADER = """\
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

<!--
  GENERATED FILE, do not edit by hand.
  Source of truth: site/content/docs/plugins/ (frontmatter + body).
  Regenerate with: python3 scripts/gen-plugins-md.py
  CI verifies this file is in sync (.github/workflows/site.yml).
-->

# Veiland plugin reference

Every first-party plugin, its config keys, types, and defaults. This is
the companion to [`config.md`](config.md): that document covers the core
schema (`name`, `binary`, `z_index`, `region`, `monitors`, `[password]`);
this one covers what goes *inside* each plugin's `[plugin.config]` table.

The complete working scenes in [`docs/examples/`](examples/) use these
keys; the website gallery shows what each scene looks like.
"""


def repo_links(text: str) -> str:
    for site, repo in LINK_MAP:
        text = text.replace(site, repo)
    return text


def split_frontmatter(path: Path) -> tuple[dict, str]:
    raw = path.read_text()
    parts = raw.split("+++", 2)
    if len(parts) != 3 or parts[0].strip():
        sys.exit(f"{path}: expected the file to start with '+++' TOML frontmatter fences")
    try:
        meta = tomllib.loads(parts[1])
    except tomllib.TOMLDecodeError as e:
        sys.exit(f"{path}: invalid TOML frontmatter: {e}")
    return meta, parts[2].strip()


def render() -> str:
    index_meta, index_body = split_frontmatter(PLUGINS_DIR / "_index.md")
    categories = index_meta["extra"]["categories"]

    pages = []
    for f in sorted(PLUGINS_DIR.glob("*.md")):
        if f.name == "_index.md":
            continue
        meta, body = split_frontmatter(f)
        pages.append((meta, body))
    pages.sort(key=lambda p: p[0]["weight"])

    # The shared prose: conventions first, everything from the stress
    # section onward goes after the per-plugin sections.
    marker = "## The stress plugin"
    if marker not in index_body:
        sys.exit(
            f"{PLUGINS_DIR / '_index.md'}: heading '{marker}' not found; the "
            "generator splits the shared prose on it. If the heading was "
            "renamed, update `marker` in this script to match."
        )
    conventions, tail = index_body.split(marker, 1)
    tail = marker + tail

    out = [HEADER, conventions.strip(), ""]

    for cat in categories:
        out.append(f"## {cat['name'].capitalize()}")
        out.append("")
        if cat.get("notes"):
            out.append(cat["notes"].strip())
            out.append("")
        for meta, body in pages:
            if meta["extra"]["category"] != cat["name"]:
                continue
            name = meta["title"]
            extra = meta["extra"]
            out.append(f"### {name} — `veiland-{name}`")
            out.append("")
            out.append(meta["description"])
            if extra.get("example"):
                out.append(f"Example: [`examples/{extra['example']}`](examples/{extra['example']}).")
            elif extra.get("used_in"):
                out.append(f"Used in [`examples/{extra['used_in']}`](examples/{extra['used_in']}).")
            out.append("")
            out.append("| Key | Type | Default | Meaning |")
            out.append("|---|---|---|---|")
            for pr in extra.get("props", []):
                out.append(
                    f"| `{pr['key']}` | {pr['type']} | {pr['default']} | {pr['meaning']} |"
                )
            out.append("")
            if body:
                out.append(body)
                out.append("")

    out.append(tail.strip())
    out.append("")
    return repo_links("\n".join(out))


def main() -> int:
    text = render()
    if "--check" in sys.argv:
        if OUT.read_text() != text:
            print(
                f"{OUT} is out of sync with site/content/docs/plugins/.\n"
                "Run: python3 scripts/gen-plugins-md.py",
                file=sys.stderr,
            )
            return 1
        print(f"{OUT} is in sync.")
        return 0
    OUT.write_text(text)
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
