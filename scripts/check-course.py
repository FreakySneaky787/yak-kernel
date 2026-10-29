#!/usr/bin/env python3
"""Checks the course (`make course`, static part also in `make docs-check`).

The course has two halves that must never drift apart:
- `course/NN-slug/`     the code of chapter NN: a complete, runnable project (Makefile with `run` and `test`)
- `docs/course/NN-slug.md`  the chapter text on the website

Static checks (always):
- every chapter folder has a page and every page a folder; chapters are numbered 01, 02, ... without gaps;
  every page is listed in docs/SUMMARY.md, in the chapter table of docs/course/README.md and in the chapter list
  of the issue form .github/ISSUE_TEMPLATE/course-stuck.yml
- every page after the first names the command that shows its changes: diff -ru course/<previous> course/<this>
- every chapter folder carries the repository's rust-toolchain.toml unchanged (learners copy single folders)
- every code block in a page whose info string names a file (```rust,file=kernel/src/main.rs) appears
  line for line in that file of the chapter folder - the text can only show code that really runs

With --build, additionally `make test` in every chapter folder (boots it in QEMU).

Usage: scripts/check-course.py [--build] [--selftest]
"""
import os
import re
import subprocess
import sys
import tempfile

CHAPTER = re.compile(r"^(\d\d)-[a-z0-9-]+$")
# an option of the chapter dropdown in the issue form: "        - 4 – GDT and TSS"
FORM_CHAPTER = re.compile(r"^\s*- (\d+) – ", re.M)
FENCE = re.compile(r"^```([^\n`]*)\n(.*?)^```\s*$", re.M | re.S)


def norm(lines):
    return [l.rstrip() for l in lines]


def contains_block(haystack, block):
    """Whether the lines of `block` occur consecutively in `haystack` (trailing spaces ignored)."""
    h, b = norm(haystack.splitlines()), norm(block.splitlines())
    while b and not b[-1]:
        b.pop()
    if not b:
        return True
    return any(h[i:i + len(b)] == b for i in range(len(h) - len(b) + 1))


def check_static(root):
    problems = []
    course_dir, pages_dir = os.path.join(root, "course"), os.path.join(root, "docs", "course")
    folders = sorted(d for d in os.listdir(course_dir) if CHAPTER.match(d)) if os.path.isdir(course_dir) else []
    pages = sorted(f[:-3] for f in os.listdir(pages_dir) if CHAPTER.match(f[:-3]) and f.endswith(".md")) \
        if os.path.isdir(pages_dir) else []
    for name in sorted(set(folders) ^ set(pages)):
        where = "page docs/course/%s.md" % name if name in folders else "folder course/%s/" % name
        problems.append(f"chapter {name}: {where} is missing")
    numbers = [int(n[:2]) for n in folders]
    if numbers != list(range(1, len(numbers) + 1)):
        problems.append(f"chapter numbers must be 01, 02, ... without gaps, found {numbers}")
    def read(*path):
        full = os.path.join(root, *path)
        return open(full, encoding="utf-8").read() if os.path.exists(full) else ""

    summary = read("docs", "SUMMARY.md")
    course_table = read("docs", "course", "README.md")
    form_chapters = {int(n) for n in FORM_CHAPTER.findall(read(".github", "ISSUE_TEMPLATE", "course-stuck.yml"))}
    toolchain = open(os.path.join(root, "rust-toolchain.toml"), encoding="utf-8").read()
    for name in folders:
        folder = os.path.join(course_dir, name)
        tc = os.path.join(folder, "rust-toolchain.toml")
        if not os.path.exists(tc) or open(tc, encoding="utf-8").read() != toolchain:
            problems.append(f"course/{name}/rust-toolchain.toml must be a copy of the repository's")
        if not os.path.exists(os.path.join(folder, "Makefile")):
            problems.append(f"course/{name}/Makefile is missing (needs `run` and `test`)")
    for i, name in enumerate(pages):
        if f"(course/{name}.md)" not in summary:
            problems.append(f"docs/course/{name}.md is not listed in docs/SUMMARY.md")
        if f"({name}.md)" not in course_table:
            problems.append(f"docs/course/{name}.md is not in the chapter table of docs/course/README.md")
        if int(name[:2]) not in form_chapters:
            problems.append(f"chapter {name} is missing from the chapter list of .github/ISSUE_TEMPLATE/course-stuck.yml")
        text = open(os.path.join(pages_dir, name + ".md"), encoding="utf-8").read()
        if i > 0 and f"diff -ru course/{pages[i - 1]} course/{name}" not in text:
            problems.append(f"docs/course/{name}.md does not name `diff -ru course/{pages[i - 1]} course/{name}`")
        for m in FENCE.finditer(text):
            info = dict(p.split("=", 1) for p in m.group(1).split(",") if "=" in p)
            if "file" not in info:
                continue
            path = os.path.join(course_dir, name, info["file"])
            line = text[:m.start()].count("\n") + 1
            if not os.path.isfile(path):
                problems.append(f"docs/course/{name}.md:{line}: {info['file']} does not exist in course/{name}/")
            elif not contains_block(open(path, encoding="utf-8").read(), m.group(2)):
                problems.append(f"docs/course/{name}.md:{line}: block is not in course/{name}/{info['file']}")
    return folders, problems


def build(root, folders):
    failed = []
    limine = os.path.join(root, "vendor", "limine")
    for name in folders:
        print(f"=== course/{name}: make test", flush=True)
        r = subprocess.run(["make", "-C", os.path.join(root, "course", name), "test", f"LIMINE_DIR={limine}"])
        if r.returncode != 0:
            failed.append(name)
    return failed


def selftest():
    with tempfile.TemporaryDirectory() as root:
        def write(path, text):
            os.makedirs(os.path.dirname(os.path.join(root, path)), exist_ok=True)
            open(os.path.join(root, path), "w", encoding="utf-8").write(text)

        os.makedirs(os.path.join(root, "course", "04-gap"))
        write("rust-toolchain.toml", "[toolchain]\n")
        for name in ("01-boot", "02-next"):
            write(f"course/{name}/rust-toolchain.toml", "[toolchain]\n")
            write(f"course/{name}/Makefile", "test:\n")
        write("course/01-boot/src/main.rs", "fn main() {\n    hello();   \n}\n")
        write("docs/SUMMARY.md", "- [1](course/01-boot.md)\n- [2](course/02-next.md)\n")
        write("docs/course/README.md", "| 1 | [Boot](01-boot.md) |\n")
        write(".github/ISSUE_TEMPLATE/course-stuck.yml", "      options:\n        - 1 – Boot\n")
        write("docs/course/01-boot.md",
              "```rust,file=src/main.rs\nfn main() {\n    hello();\n}\n```\n"
              "```rust,file=src/main.rs\nfn other() {}\n```\n"
              "```rust\nnot checked\n```\n")
        write("docs/course/02-next.md", "diff -ru course/01-other course/02-next\n")
        _, problems = check_static(root)
    expected = ["chapter 04-gap: page docs/course/04-gap.md is missing",
                "chapter numbers must be 01, 02, ... without gaps, found [1, 2, 4]",
                "course/04-gap/rust-toolchain.toml must be a copy of the repository's",
                "course/04-gap/Makefile is missing (needs `run` and `test`)",
                "docs/course/01-boot.md:6: block is not in course/01-boot/src/main.rs",
                "docs/course/02-next.md is not in the chapter table of docs/course/README.md",
                "chapter 02-next is missing from the chapter list of .github/ISSUE_TEMPLATE/course-stuck.yml",
                "docs/course/02-next.md does not name `diff -ru course/01-boot course/02-next`"]
    ok = problems == expected
    print("check-course selftest:", "ok" if ok else f"FAILED {problems}")
    return 0 if ok else 1


def main():
    if "--selftest" in sys.argv[1:]:
        return selftest()
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    folders, problems = check_static(root)
    for p in problems:
        print("FAIL ", p)
    print(f"check-course: {len(folders)} chapters, {len(problems)} problems")
    if problems:
        return 1
    if "--build" in sys.argv[1:]:
        failed = build(root, folders)
        print(f"check-course: {len(folders) - len(failed)}/{len(folders)} chapters pass `make test`"
              + (f" (failed: {', '.join(failed)})" if failed else ""))
        return 1 if failed else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
