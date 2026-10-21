#!/usr/bin/env python3
"""Build the guide and Rust API reference for GitHub Pages."""

from __future__ import annotations

import argparse
import importlib.util
from html.parser import HTMLParser
from html import escape, unescape
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "target" / "docs-python"))
try:
    from pygments import highlight
    from pygments.formatters import HtmlFormatter
    from pygments.lexers import RustLexer, TOMLLexer
except ImportError as error:
    raise SystemExit("Install documentation tools with: python -m pip install -r docs/requirements.txt") from error
PAGE_TITLES = {
    "start": "Quick start", "sessions": "Sessions", "lifecycle": "Start, stop, run",
    "configuration": "Configuration", "messages": "Messages", "errors": "Errors", "updates": "Updates",
    "files": "Files", "features": "Feature flags", "architecture": "Crates",
    "development": "Development",
}


class Links(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.targets: list[str] = []
        self.ids: set[str] = set()
        self.base: str | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag == "base":
            self.base = values.get("href")
            return
        if values.get("id"):
            self.ids.add(values["id"])
        for key in ("href", "src", "action"):
            if values.get(key):
                self.targets.append(values[key])


def verify_guide(output: Path, page: Path, skip_api: bool) -> None:
    links = Links()
    links.feed(page.read_text(encoding="utf-8"))
    base = (page.parent / (links.base or "")).resolve()
    if not base.is_relative_to(output):
        raise ValueError(f"Page base escapes its site: {page.name}")
    for target in links.targets:
        if target.startswith(("https://", "http://", "mailto:")):
            continue
        if target.startswith("#"):
            if target[1:] and target[1:] not in links.ids:
                raise ValueError(f"Unknown guide anchor: {target}")
            continue
        if skip_api and target.startswith(("api/", "methods")):
            continue
        path = (base / target.split("#", 1)[0].split("?", 1)[0]).resolve()
        if not path.is_relative_to(output):
            raise ValueError(f"Guide link escapes its site: {target}")
        if path.is_dir():
            path /= "index.html"
        if not path.is_file():
            raise ValueError(f"Missing guide target: {target}")


def build_guides(output: Path) -> list[Path]:
    template = (output / "index.html").read_text(encoding="utf-8")
    sections = re.findall(r'(<section id="([^"]+)"[^>]*>.*?</section>)', template, re.S)
    if [key for _, key in sections] != list(PAGE_TITLES):
        raise ValueError("Guide sections and navigation definitions differ")
    paths = {key: "index.html" if key == "start" else f"{key}.html" for key in PAGE_TITLES}
    footer = re.search(r"<footer>.*?</footer>", template, re.S).group()
    pages = []
    for index, (section, key) in enumerate(sections):
        if key != "start":
            section = re.sub(r"<h2>(.*?)</h2>", r"<h1>\1</h1>", section, count=1)
        headings = []

        def heading(match: re.Match[str]) -> str:
            level, text = match.groups()
            label = unescape(re.sub(r"<[^>]+>", "", text))
            slug = re.sub(r"[^a-z0-9]+", "-", label.lower()).strip("-")
            headings.append((slug, label))
            return f'<h{level} id="{slug}">{text}</h{level}>'

        section = re.sub(r"<h([23])>(.*?)</h\1>", heading, section)
        crumbs = f'<nav class="breadcrumbs" aria-label="Breadcrumb"><a href="index.html">Docs</a><span>/</span><span>{PAGE_TITLES[key]}</span></nav>'
        adjacent = '<nav class="page-navigation" aria-label="Guide pages">'
        for offset, label in ((-1, "Previous"), (1, "Next")):
            neighbor = index + offset
            if 0 <= neighbor < len(sections):
                neighbor_key = sections[neighbor][1]
                adjacent += f'<a href="{paths[neighbor_key]}"><span>{label}</span>{PAGE_TITLES[neighbor_key]}</a>'
        adjacent += "</nav>"
        page = re.sub(r'<main id="main">.*?</main>', lambda _: f'<main id="main">{crumbs}{section}{adjacent}{footer}</main>', template, flags=re.S)
        for anchor, name in paths.items():
            current = ' aria-current="page"' if anchor == key else ""
            page = page.replace(f'href="#{anchor}"', f'href="{name}"{current}')
        toc = '<nav aria-label="On this page"><p>On this page</p>'
        toc += "".join(f'<a href="#{slug}">{escape(label)}</a>' for slug, label in headings)
        toc += "</nav>" if headings else "</nav>"
        page = page.replace("<!-- PAGE_TOC -->", toc if headings else "")
        page = page.replace("<title>grammers documentation</title>", f"<title>{PAGE_TITLES[key]} · grammers</title>")
        path = output / paths[key]
        path.write_text(highlight_blocks(page), encoding="utf-8")
        pages.append(path)
    return pages


def highlight_blocks(page: str) -> str:
    def render(match: re.Match[str]) -> str:
        attributes, code = match.groups()
        lexer = TOMLLexer if "language-toml" in attributes else RustLexer
        rendered = highlight(unescape(code), lexer(stripnl=False, ensurenl=False), HtmlFormatter(nowrap=True, style="github-dark"))
        return f"<pre><code{attributes}>{rendered}</code></pre>"

    return re.sub(r"<pre><code([^>]*)>(.*?)</code></pre>", render, page, flags=re.S)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    api_options = parser.add_mutually_exclusive_group()
    api_options.add_argument("--skip-api", action="store_true", help="Preview the guide without compiling rustdoc")
    api_options.add_argument("--reuse-api", action="store_true", help="Reuse an already verified target/doc build")
    args = parser.parse_args()
    target = (ROOT / "target").resolve()
    output = (target / "site").resolve()
    if not target.is_relative_to(ROOT) or output.parent != target:
        raise ValueError("Refusing to replace a docs directory outside this checkout's target directory")

    if not args.skip_api and not args.reuse_api:
        env = os.environ.copy()
        env["RUSTDOCFLAGS"] = f'{env.get("RUSTDOCFLAGS", "")} -D warnings --default-theme dark'.strip()
        subprocess.run(
            ["cargo", "doc", "--locked", "--workspace", "--all-features", "--no-deps", "--target-dir", str(target)],
            cwd=ROOT, env=env, check=True,
        )

    # Keep the output directory itself: a preview server may use it as its
    # working directory, which prevents removing it on Windows.
    output.mkdir(parents=True, exist_ok=True)
    for entry in output.iterdir():
        if entry.is_symlink():
            entry.unlink()
        elif entry.is_dir():
            if not entry.resolve().is_relative_to(output):
                raise ValueError("Refusing to remove a linked directory outside the generated site")
            shutil.rmtree(entry)
        else:
            entry.unlink()
    shutil.copytree(ROOT / "docs" / "site", output, dirs_exist_ok=True)
    (output / "assets").mkdir(exist_ok=True)
    for name in ("architecture-dark.png", "logo.png", "github.svg"):
        shutil.copy2(ROOT / "assets" / name, output / "assets" / name)
    token_css = HtmlFormatter(style="github-dark").get_style_defs(".code")
    (output / "syntax.css").write_text("\n".join(line for line in token_css.splitlines() if line.startswith(".code .")), encoding="utf-8")
    if not args.skip_api:
        shutil.copytree(target / "doc", output / "api")
    (output / ".nojekyll").touch()
    pages = build_guides(output)
    if not args.skip_api:
        spec = importlib.util.spec_from_file_location("grammers_api_docs", ROOT / "scripts" / "enrich-api-docs.py")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        coverage = module.enrich(output, ROOT)
        print(f"API method pages: {coverage}")
        pages.extend(output.glob("methods/**/*.html"))
        pages.append(output / "methods.html")
    for page in pages:
        verify_guide(output, page, args.skip_api)
    print(f"Documentation built: {output}")
    if args.skip_api:
        print("Guide preview only; API links require the full build.")


if __name__ == "__main__":
    main()
