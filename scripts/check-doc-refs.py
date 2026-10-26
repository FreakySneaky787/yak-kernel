#!/usr/bin/env python3
"""Checks the code references in the documentation (run by `make check`).

Every Markdown link to a file in the repository must point to an existing
file. A link whose text is a code span, [`name`](path), additionally promises
that `name` occurs in that file: for `a::b` or `A::b` the last part must occur,
for `fn x` / `x()` the identifier `x`. Links to URLs and anchors are skipped.

Usage: scripts/check-doc-refs.py [--selftest]
"""
import os
import re
import sys
import tempfile

LINK = re.compile(r"(?<!\\)\[(`[^`]+`|[^\]]+)\]\(([^)\s]+)\)")


def symbol_of(text):
    """The identifier a code-span link text promises, or None for plain text."""
    if not (text.startswith("`") and text.endswith("`")):
        return None
    name = text[1:-1].strip()
    name = re.sub(r"\(.*\)$", "", name)          # x() -> x
    name = re.sub(r"^(fn|struct|enum|const|static|mod)\s+", "", name)
    name = name.split("::")[-1]                  # a::b -> b
    name = name.rstrip("!")                      # macro! -> macro
    if "/" in name or "." in name or " " in name:
        return None                              # a file name like `boot.rs`, not a symbol
    return name or None


def check_file(md_path, root):
    problems = []
    base = os.path.dirname(md_path)
    with open(md_path, encoding="utf-8") as f:
        text = f.read()
    for line_no, line in enumerate(text.splitlines(), 1):
        for m in LINK.finditer(line):
            label, target = m.group(1), m.group(2)
            if re.match(r"^[a-z]+:", target) or target.startswith("#"):
                continue
            path = os.path.normpath(os.path.join(base, target.split("#")[0]))
            where = f"{os.path.relpath(md_path, root)}:{line_no}"
            if not os.path.exists(path):
                problems.append(f"{where}: {target} does not exist")
                continue
            name = symbol_of(label)
            if name and os.path.isfile(path):
                with open(path, encoding="utf-8", errors="replace") as code:
                    if not re.search(r"(?<![A-Za-z0-9_])" + re.escape(name) + r"(?![A-Za-z0-9_])", code.read()):
                        problems.append(f"{where}: `{name}` not found in {target}")
    return problems


def check_tree(root):
    files = [os.path.join(root, "README.md")]
    for d, _, names in os.walk(os.path.join(root, "docs")):
        files += [os.path.join(d, n) for n in names if n.endswith(".md")]
    problems, links = [], 0
    for f in sorted(files):
        if os.path.exists(f):
            problems += check_file(f, root)
            links += sum(len(LINK.findall(l)) for l in open(f, encoding="utf-8"))
    return problems, links, len(files)


def selftest():
    with tempfile.TemporaryDirectory() as root:
        os.makedirs(os.path.join(root, "docs"))
        with open(os.path.join(root, "code.rs"), "w") as f:
            f.write("pub fn present() {}\n")
        with open(os.path.join(root, "README.md"), "w") as f:
            f.write("[`present`](code.rs) [`fn present()`](code.rs) [`absent`](code.rs) [x](nofile.rs) [w](https://x)\n")
        problems, _, _ = check_tree(root)
    ok = len(problems) == 2 and "`absent`" in problems[0] and "nofile.rs" in problems[1]
    print("check-doc-refs selftest:", "ok" if ok else f"FAILED {problems}")
    return 0 if ok else 1


def main():
    if sys.argv[1:] == ["--selftest"]:
        return selftest()
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    problems, links, files = check_tree(root)
    for p in problems:
        print("FAIL ", p)
    print(f"check-doc-refs: {links} links in {files} files, {len(problems)} broken")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
