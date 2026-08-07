#!/usr/bin/env python3
"""Render site/changelog.html from docs/releases artifacts."""
from __future__ import annotations

import html
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RELEASES = ROOT / "docs" / "releases"
OUT = ROOT / "site" / "changelog.html"

SHELL = """\
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Powder release notes</title>
    <meta name="description" content="User-facing release notes for Powder." />
    <script>
      try {{
        var m = localStorage.getItem('ae-mode');
        if (m === 'dark' || m === 'light') {{
          document.documentElement.classList.add(m);
          document.documentElement.style.colorScheme = m;
        }}
      }} catch (e) {{}}
    </script>
    <link rel="stylesheet" href="aesthetic.css" />
    <link rel="stylesheet" href="marketing.css" />
  </head>
  <body>
    <div class="ae-shell msk-shell">
      <header class="ae-bar msk-nav">
        <a class="msk-brand" href="index.html">Powder</a>
        <nav class="msk-links" aria-label="Marketing">
          <a href="features.html">features</a>
          <a href="get-started.html">get started</a>
          <a href="changelog.html" aria-current="page">changelog</a>
        </nav>
      </header>
      <main class="ae-stage ae-stage-scroll">
        <article class="ae-doc msk-page" aria-labelledby="release-notes-title">
          <h1 id="release-notes-title">Release notes</h1>
          <p class="ae-lede">
            Generated from <code>docs/releases/</code> (Landmark artifacts).
          </p>
{sections}
        </article>
      </main>
      <footer class="ae-bar msk-footer">
        <button class="ae-mode" aria-label="toggle color mode"></button>
        <p class="ae-chrome msk-credit">
          a <a href="https://mistystep.io">Misty Step</a> project
        </p>
      </footer>
    </div>
    <script src="mode.js"></script>
  </body>
</html>
"""


def entries() -> list[dict]:
    catalog = RELEASES / "releases.json"
    if catalog.is_file() and catalog.stat().st_size > 1:
        data = json.loads(catalog.read_text())
        if isinstance(data, list) and data:
            return [e for e in data if isinstance(e, dict)]
    return [
        {"version": p.stem, "path": str(p.relative_to(ROOT))}
        for p in sorted(RELEASES.glob("*.md"), reverse=True)
    ]


def md_body(text: str) -> str:
    out: list[str] = []
    ul = False
    para: list[str] = []

    def end_p() -> None:
        if para:
            out.append("<p>" + html.escape(" ".join(para)) + "</p>")
            para.clear()

    def end_ul() -> None:
        nonlocal ul
        if ul:
            out.append("</ul>")
            ul = False

    for line in text.splitlines():
        if line.startswith("#"):
            end_p()
            end_ul()
            level = len(line) - len(line.lstrip("#"))
            title = line.lstrip("#").strip()
            if level <= 1:
                continue
            tag = "h2" if level == 2 else "h3"
            out.append(f"<{tag}>{html.escape(title)}</{tag}>")
        elif re.match(r"^\s*-\s+", line):
            end_p()
            if not ul:
                out.append("<ul>")
                ul = True
            item = re.sub(r"^\s*-\s+", "", line).strip()
            out.append(f"<li>{html.escape(item)}</li>")
        elif not line.strip():
            end_p()
            end_ul()
        else:
            end_ul()
            para.append(line.strip())
    end_p()
    end_ul()
    return "\n".join(out)


def section(entry: dict) -> str:
    version = str(entry.get("version") or entry.get("tag") or "unknown")
    path = ROOT / (entry.get("path") or f"docs/releases/{version}.md")
    body = path.read_text() if path.is_file() else ""
    body = re.sub(r"^# .*\n+", "", body, count=1)
    published = str(entry.get("published_at") or "")
    chrome = html.escape(f"{published[:10] + ' - ' if published else ''}{version}")
    return (
        '          <section class="msk-release">\n'
        f'            <p class="ae-chrome">{chrome}</p>\n'
        f"{md_body(body)}\n"
        "          </section>"
    )


def main() -> None:
    rows = entries()
    sections = "\n".join(section(e) for e in rows) or (
        '          <section class="msk-release">'
        "<p class=\"ae-lede\">No releases in docs/releases yet.</p>"
        "</section>"
    )
    OUT.write_text(SHELL.format(sections=sections))
    print(f"wrote {OUT} ({len(rows)} entr{'y' if len(rows) == 1 else 'ies'})")


if __name__ == "__main__":
    main()
