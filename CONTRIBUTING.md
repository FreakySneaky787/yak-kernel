# Contributing to YAK

YAK is a hobby kernel and a course in writing one. I (FreakySneaky787) steer it; the code is written together with
an AI. Reports and fixes from readers are very welcome – the most valuable thing right now is somebody working
through [the course](docs/course/README.md) and telling me where it stops making sense.

## Reporting a problem
- **Stuck in a course chapter** – a build fails, a test is red, or the page and the code disagree: open an issue with
  the form "I got stuck in a course chapter". It asks for the chapter, the step and the output.
- **Feedback on the course** – something was unclear, too long or in the wrong order, even if it worked: the form
  "Feedback on the course".
- **A bug in the kernel itself** (`kernel/`, `user/`): a normal issue with what you ran, what you expected, the
  serial output, and – for a panic – the RIP mapped to a line (`make addr2line ADDR=…`). [`docs/BUGS.md`](docs/BUGS.md)
  lists every bug so far; many symptoms have happened before.

## Sending a change
1. One topic per pull request: a fix, a chapter correction, a new test.
2. Run the checks that cover what you changed:
   - kernel or user programs: `make check` (unit tests, integration test kernels, shell E2E with BIOS and UEFI);
   - a course chapter: `make test` in its folder, then `make course` (all chapters) and `make docs-check`;
   - documentation: `make docs-check`, and `make book` if you have `mdbook`.
3. A bug fix gets an entry in `docs/BUGS.md` (symptom, cause, fix, test) and, where possible, a regression test.
4. Everything in the repository is English: code, comments, kernel output, docs, commit messages.
5. A fix in a course chapter that later chapters share goes into all of them – each chapter folder is a complete
   copy of the kernel at that point.

[`docs/HACKING.md`](docs/HACKING.md) explains how to add a syscall, a program, a test or a course chapter.

## The rules the project keeps
- **From scratch:** GDT, IDT, paging, allocators and drivers are the project's own code. The only crates are
  `limine` (boot protocol) and `spin` (locks); please ask in an issue before proposing another one.
- **No claim without proof:** a sentence on a course page that says what happens is backed by a test or by a run
  whose output is on the page.

## License
YAK is licensed under the GNU General Public License v3 or later ([`LICENSE`](LICENSE)). By sending a change you
agree that it is distributed under the same license.
