# Making a release

A release is a version number, a git tag and files anybody can run with nothing but QEMU. This page is the
checklist; every step names the command that proves it. Claude prepares everything up to the pull request; I create
the tag and the GitHub release (tags cannot be pushed from Claude's sandbox).

## Before the release
1. **Version** in `kernel/Cargo.toml` and `user/Cargo.toml` (both the same; `scripts/release.sh` refuses otherwise).
   The boot banner and `uname` show it.
2. **`CHANGELOG.md`**: the "Unreleased" section becomes `## <version> – <date>`.
3. **Tests**, on the commit that becomes the release:
   - `make check` – unit and integration tests, end-to-end shell sessions under BIOS and UEFI;
   - `make stress` – the suite 5 times with 1, 2 and 4 CPUs, `qemu64`, UEFI, then the soak test (about 30 minutes
     under TCG); the result goes into the log in `docs/PROGRESS.md`;
   - `make course` – every course chapter builds and passes its test;
   - `make book` – the website builds without broken links;
   - `make release` – the release files are built and booted once (BIOS and UEFI).
4. **Known limitations** in `docs/PROGRESS.md` are current: everything that does not work is listed there.
5. Pull request; CI must be green on Ubuntu and Fedora; merge.

## The release
6. On GitHub: **Releases → Draft a new release**, tag `v<version>` on the merge commit of `main`, title
   `YAK <version>`, text from the `CHANGELOG.md` section; for a version before 1.0 tick "Set as a pre-release".
   Publish.
7. The workflow **release** (`.github/workflows/release.yml`) starts by itself: it checks out the tag, runs
   `make release` (which refuses a tag that does not match the version) and attaches `yak-<version>.iso`,
   `yak-<version>-disk.img.gz`, `run-yak.sh` and `SHA256SUMS` to the release. If it fails, nothing is attached;
   it can be run again by hand (Actions → release → Run workflow, with the tag).
8. **Check the download**: the release page shows the four files; on a machine with QEMU,
   `sha256sum -c SHA256SUMS` and `./run-yak.sh` (README, "Try it without building").

## What a release promises
What `make check` tests, on the machines in README "Supported environment" – nothing more. Everything else is either
a documented limitation (`docs/PROGRESS.md`) or a bug (`docs/BUGS.md`).
