#!/usr/bin/env python3
"""Builds the website (`make book`): mdBook over docs/ into build/book, then fixes the links.

The docs link to source files (`../kernel/src/...`) and to files outside docs/ (`../README.md`). On GitHub those
links work; on the website they would point outside the site. After `mdbook build`, every relative link whose target
is a repository file outside docs/ becomes a link to that file on GitHub. Finally every relative link left in the
generated pages must resolve to a file of the site - otherwise the build fails.

Usage: scripts/build-book.py [--selftest]
"""
import html
import os
import re
import shutil
import subprocess
import sys
import tempfile

REPO_URL = "https://github.com/FreakySneaky787/yak-kernel"
BRANCH = "main"
HREF = re.compile(r'(href|src)="([^"]+)"')


def rewrite_page(page, out_dir, src_dir, root):
    """Rewrites the links of one generated page; returns the relative links that stay on the site."""
    rel_page = os.path.relpath(page, out_dir)                # walkthrough/01-boot.html
    src_page_dir = os.path.join(src_dir, os.path.dirname(rel_page))
    kept = []

    def fix(m):
        attr, target = m.group(1), html.unescape(m.group(2))
        if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith(("#", "/", "data:")):
            return m.group(0)
        path, _, anchor = target.partition("#")
        if not path:
            return m.group(0)
        repo_path = os.path.normpath(os.path.join(src_page_dir, path))
        if path.endswith(".html") and not os.path.exists(repo_path) and os.path.exists(repo_path[:-5] + ".md"):
            repo_path = repo_path[:-5] + ".md"            # mdBook turned a link to a .md file into .html
        inside_docs = os.path.commonpath([repo_path, src_dir]) == src_dir
        if inside_docs and os.path.basename(path) == "README.html":
            # mdBook publishes a chapter's README.md as index.html
            path = path[: -len("README.html")] + "index.html"
            kept.append(path)
            return f'{attr}="{html.escape(path + ("#" + anchor if anchor else ""))}"'
        if not inside_docs and os.path.exists(repo_path):
            kind = "tree" if os.path.isdir(repo_path) else "blob"
            url = f"{REPO_URL}/{kind}/{BRANCH}/{os.path.relpath(repo_path, root)}"
            if anchor:
                url += "#" + anchor
            return f'{attr}="{html.escape(url)}"'
        kept.append(path)
        return m.group(0)

    with open(page, encoding="utf-8") as f:
        text = f.read()
    text = HREF.sub(fix, text)
    with open(page, "w", encoding="utf-8") as f:
        f.write(text)
    return kept


def fix_and_check(out_dir, src_dir, root):
    broken = []
    pages = 0
    for d, _, names in os.walk(out_dir):
        for n in names:
            if not n.endswith(".html"):
                continue
            page = os.path.join(d, n)
            pages += 1
            for path in rewrite_page(page, out_dir, src_dir, root):
                target = os.path.normpath(os.path.join(d, path))
                if not os.path.exists(target):
                    broken.append(f"{os.path.relpath(page, out_dir)}: {path}")
    return pages, sorted(set(broken))


def selftest():
    with tempfile.TemporaryDirectory() as root:
        src, out = os.path.join(root, "docs"), os.path.join(root, "out")
        os.makedirs(os.path.join(root, "kernel"))
        os.makedirs(src)
        os.makedirs(out)
        open(os.path.join(root, "kernel", "a.rs"), "w").close()
        open(os.path.join(out, "other.html"), "w").close()
        with open(os.path.join(out, "page.html"), "w") as f:
            f.write('<a href="../kernel/a.rs#x">a</a> <a href="other.html">o</a> <a href="gone.html">g</a> '
                    '<a href="https://x">w</a> <a href="../NOTES.html">n</a> <a href="README.html">r</a>')
        open(os.path.join(root, "NOTES.md"), "w").close()
        open(os.path.join(out, "index.html"), "w").close()
        pages, broken = fix_and_check(out, src, root)
        text = open(os.path.join(out, "page.html")).read()
    ok = (pages == 3 and broken == ["page.html: gone.html"]
          and f'href="{REPO_URL}/blob/{BRANCH}/NOTES.md"' in text and 'href="index.html"' in text
          and f'href="{REPO_URL}/blob/{BRANCH}/kernel/a.rs#x"' in text and 'href="other.html"' in text)
    print("build-book selftest:", "ok" if ok else f"FAILED pages={pages} broken={broken} text={text}")
    return 0 if ok else 1


def main():
    if sys.argv[1:] == ["--selftest"]:
        return selftest()
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    mdbook = shutil.which("mdbook") or os.path.expanduser("~/.cargo/bin/mdbook")
    if not os.path.exists(mdbook):
        print("mdbook not found - install it with: cargo install mdbook --locked --version 0.4.52")
        return 1
    out_dir = os.path.join(root, "build", "book")
    shutil.rmtree(out_dir, ignore_errors=True)
    subprocess.run([mdbook, "build", root], check=True)
    pages, broken = fix_and_check(out_dir, os.path.join(root, "docs"), root)
    for b in broken:
        print("FAIL  broken link in", b)
    print(f"build-book: {pages} pages in build/book, {len(broken)} broken links")
    return 1 if broken else 0


if __name__ == "__main__":
    sys.exit(main())
