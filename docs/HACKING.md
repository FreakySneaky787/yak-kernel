# Extending KernelTry

Recipes for the four most common changes. Each one ends with the tests that prove it works – a change without a test
is not finished (see `CLAUDE.md`). Run `make check` before every commit.

## Add a system call
Example in the tree: `uname` (syscall 52), added by following exactly these steps.

1. **Number** – in [`syscall.rs`](../kernel/src/process/syscall.rs), module `nr`: add a doc comment with the signature
   and the result, and the next free number. Never reuse or renumber: user programs on existing disks use the numbers.
2. **Dispatch** – one arm in [`dispatch`](../kernel/src/process/syscall.rs): `nr::UNAME => sys_uname(a0, a1),`.
   The arguments arrive as `a0`…`a4` (RDI, RSI, RDX, R10, R8).
3. **Implementation** – a `fn sys_<name>(…) -> SysResult` next to its relatives. The rules every syscall follows:
   - **Never dereference a user pointer.** Use [`copy_from_user`](../kernel/src/process/syscall.rs),
     [`copy_to_user`](../kernel/src/process/syscall.rs), [`copy_struct_to_user`](../kernel/src/process/syscall.rs),
     [`user_path`](../kernel/src/process/syscall.rs). They check the range against the page tables and copy under the
     process lock (bugs #12, #14). An unchecked access is a kernel crash waiting for the wrong argument.
   - **Errors are negative `errno` values** (`Err(errno::EFAULT)`); add a new code to module `errno` *and* to
     the messages of `impl Display for Error` in [`fs.rs`](../user/src/fs.rs) if you need one.
   - **Check first, then change state.** A call that fails should leave nothing half-done (bug #13).
   - **Bound every loop by something the kernel owns**, never by a user-supplied length alone (bug #11).
   - Blocking calls wait with [`wait_for`](../kernel/src/task/mod.rs) and give up when
     [`signal_pending`](../kernel/src/process/mod.rs) is true (`EINTR`), so Ctrl+C and `kill` still work.
   - Do not hold the process table lock (`with_current`) while blocking or doing disk I/O.
4. **User side** – in [`sys.rs`](../user/src/sys.rs): the same number as `pub const`, bump
   [`SYSCALL_COUNT`](../user/src/sys.rs), and a safe wrapper (`pub fn uname(buf: &mut [u8]) -> i64`). Higher-level
   helpers belong in the other `ulib` modules (`fs`, `process`, …).
5. **Robustness test** – add the call to [`syscheck`](../user/src/bin/syscheck.rs): its bad-pointer loop (every pointer
   argument × every bad pointer and length must return an error) and, if it never blocks, the random phase list
   `RANDOM_OK`. `SYSCALL_COUNT` moves the "unknown number" checks automatically.
6. **Functional test** – call it from a program (step 2 of "Add a user program") and check the output in the
   integration test, or write checks into a self-test program.
7. **Document** – a row in the syscall table in `docs/DESIGN.md`.

## Add a user program
1. Create `user/src/bin/<name>.rs` (or `user/src/bin/<name>/main.rs` for several files), starting like
   [`uname.rs`](../user/src/bin/uname.rs):
   ```rust
   #![no_std]
   #![no_main]
   use ulib::println;
   ulib::entry!(main);
   fn main(args: ulib::Args) -> i32 { println!("hi"); 0 }
   ```
   `extern crate alloc;` if you need `Vec`/`String` (the heap is ready before `main`). The first line is a `//!`
   comment with a usage line – `help` and readers rely on it.
2. Nothing to register: the Makefile finds every program (`USER_PROGRAMS`), `make user` builds it, and every disk image
   gets it in `/bin` (`scripts/mkdisk.sh`, `scripts/install-user.sh`). Type its name in the shell.
3. Test it: a `run("<name>", "args")` with assertions in [`userspace.rs`](../kernel/tests/userspace.rs), and an E2E
   step in [`e2e-shell.py`](../scripts/e2e-shell.py) if it matters at the shell prompt.
4. Add it to the program list in `CLAUDE.md` (layout section).

## Add a test
| Kind | Where | Runs | Use it for |
|---|---|---|---|
| unit test | `#[test_case] fn …()` in a `#[cfg(test)] mod tests` of the module | `cargo test --lib` inside the kernel | functions and subsystems with the real hardware of QEMU |
| integration test | new file `kernel/tests/<name>.rs` (copy [`page_fault.rs`](../kernel/tests/page_fault.rs) for an own `kmain`), plus `[[test]] name = "<name>" harness = false` in `kernel/Cargo.toml` if it must fault or exit itself | its own kernel image | boot-time behaviour, faults, memory reclaim, real programs |
| user self-test | a program like [`pipetest.rs`](../user/src/bin/pipetest.rs): print `FAIL: …` per failed check, exit code = number of failures | from the integration test and E2E | syscalls from user space |
| E2E step | a tuple `(keys, expected, forbidden)` in `STEPS` of [`e2e-shell.py`](../scripts/e2e-shell.py) | `make e2e`, `make e2e-uefi` | what a user types and sees |

All of them run headless with a 30 s timeout per kernel (`KERNEL_TEST_TIMEOUT`). A test that needs more than one CPU
should return early when `smp::online_count()` is 1, so `KERNEL_SMP=1 make test` stays green. Timing assertions need
a generous margin – the suite also runs under `make stress` on a loaded machine.

For a bug fix: write the test first, watch it fail on the old code, then fix (see `docs/BUGS.md` – every entry names its test).

## Add a shell built-in
Built-ins are commands that must run *inside* the shell process (they change its state: `cd`, `export`, `exit`).
Everything else should be a program.
1. Add `(name, usage, help)` to `BUILTINS` in [`main.rs`](../user/src/bin/sh/main.rs) – `help` and tab completion use it.
2. Add an arm to the `match` in `run_stage` (next to `"pwd"`); return `Stage::Done(exit_code)`.
3. Test it in the scripted `sh` sessions of [`userspace.rs`](../kernel/tests/userspace.rs) (injected serial input) or
   with an E2E step.

## When something goes wrong
- `docs/BUGS.md` – most symptoms have happened before.
- `make addr2line ADDR=…` for a RIP from a panic; `make debug` + `make gdb` for stepping.
- Triple fault / reboot loop: `-d int,cpu_reset -D log.txt` on the `scripts/run-qemu.sh` command line.
- Interrupts not arriving: `python3 -I scripts/qemu-monitor.py <elf> 5 -- "info pic" "info lapic"`.
