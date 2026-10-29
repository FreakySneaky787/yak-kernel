# 2 – Testing without an operating system

**Goal:** unit tests that run *inside* the kernel, started with a plain `cargo test`.
**You end up with:** `make test` builds a test kernel, boots it in QEMU, runs every test and reports PASS or FAIL
through QEMU's exit status – no human has to look at the output.
**Code:** [`course/02-tests/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/02-tests) (chapter 1 plus
`testing.rs`, `qemu.rs` and a new `run-qemu.sh`)
**What changed:** `diff -ru course/01-boot course/02-tests` (in the repository) shows every line this chapter adds.

## Why this comes so early
Everything later – memory management, the scheduler, the file system – is easier to get right with tests, and much
harder to debug without them. A kernel bug often shows up far from its cause. The reference kernel has over 200 such
tests; every one of them uses what this chapter builds.

## Background: the problem
`cargo test` normally builds a program that contains your tests and the standard test harness, and runs it on your
machine. Neither works for us: the harness needs `std`, and our code only runs on bare metal. Rust has an unstable
feature for exactly this case, **custom test frameworks**: the compiler still finds all tests, but hands them to a
function *we* write.

## Step 1: let the compiler collect the tests
Three lines at the top of `main.rs`:

```rust,file=kernel/src/main.rs
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]
```

- Tests are marked `#[test_case]` (not `#[test]`).
- In a test build, the compiler generates a function that calls `testing::runner` with a slice of all of them,
  and names it `test_main`, so that we can call it.

The test build needs its own entry point – it should run the tests, not greet us:

```rust,file=kernel/src/main.rs
/// The entry point of the test build (`cargo test`): run every `#[test_case]`.
#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    test_main();
    halt() // not reached: the runner ends QEMU
}
```

The normal `kmain` gets `#[cfg(not(test))]`, so exactly one of the two exists in each build. In `Cargo.toml` the
`[[bin]]` section no longer says `test = false` – otherwise `cargo test` would skip our crate.

## Step 2: the runner
`testing.rs` turns every test into something that prints its own name:

```rust,file=kernel/src/testing.rs
impl<T: Fn()> Testable for T {
    fn run(&self) {
        serial_print!("{} ... ", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}
```

`core::any::type_name` gives the full path of the function, for example `kernel::testing::trivial_assertion`. The
runner runs them one after another:

```rust,file=kernel/src/testing.rs
pub fn runner(tests: &[&dyn Testable]) -> ! {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    serial_println!("All tests passed.");
    qemu::exit(ExitCode::Success)
}
```

A failing `assert!` panics – so in a test build, the **panic handler** is where a test fails. It prints the message
and ends QEMU with the failure code:

```rust,file=kernel/src/testing.rs
pub fn panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}", info);
    qemu::exit(ExitCode::Failed)
}
```

(`serial_print!` is new in `serial.rs`: `serial_println!` without the line break.)

## Step 3: telling the outside world – `isa-debug-exit`
A test that only prints its result still needs someone to read it. QEMU has a tiny device for this: **isa-debug-exit**.
Writing a number to its I/O port ends QEMU immediately, with an exit status derived from the number:

```rust,file=kernel/src/qemu.rs
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum ExitCode {
    Success = 0x10, // QEMU exits with 33
    Failed = 0x11,  // QEMU exits with 35
}
```

QEMU exits with `(value << 1) | 1` – always odd, so a status can never be confused with QEMU's own `0` (normal
end) or `1` (error). We picked `0x10` and `0x11`; any two values work. The device only exists when QEMU is started
with `-device isa-debug-exit,iobase=0xf4,iosize=0x04`.

## Step 4: `cargo test` boots QEMU
Cargo can hand a built program to a **runner** instead of executing it. In `kernel/.cargo/config.toml`:

```toml,file=kernel/.cargo/config.toml
# `cargo run` and `cargo test` hand the built kernel to this script, which boots it in QEMU
runner = "../run-qemu.sh"
```

`run-qemu.sh` now has to know whether it got the normal kernel (`cargo run`) or a test kernel (`cargo test`). Cargo
names test binaries `kernel-` followed by a 16-digit hash, so the script looks at the file name:

```bash,file=run-qemu.sh
if [[ -z "${KERNEL_MODE:-}" ]]; then
    if [[ "$(basename "$ELF")" =~ -[0-9a-f]{16}$ ]]; then KERNEL_MODE=test; else KERNEL_MODE=run; fi
fi
```

In test mode it starts QEMU without a window, with a timeout, and translates the exit status:

```bash,file=run-qemu.sh
    case $code in
        33)  echo "[test] PASS"; exit 0 ;;  # 0x10: all tests passed
        124) echo "[test] FAIL: timeout after ${TIMEOUT}s - a test hangs?"; exit 1 ;;
        *)   echo "[test] FAIL: QEMU exit status $code"; exit 1 ;;  # 35 = 0x11: a test failed
    esac
```

The timeout matters: a test that hangs must fail too, or nobody notices. The check from chapter 1 (the normal
kernel prints the lines of `expected.txt`) is still there as `KERNEL_MODE=check`.

## Run it
```sh
cd course/02-tests
make test
```
```text
Running 3 tests
kernel::limine_supports_our_base_revision ... [ok]
kernel::serial::long_lines_are_sent_completely ... 0123456789abcdef0123456789abcdef…
[ok]
kernel::testing::trivial_assertion ... [ok]
All tests passed.
[test] PASS
[check] PASS - the serial output contains every line of expected.txt
```

Now break a test – change `assert_eq!(1 + 1, 2)` in `testing.rs` to `3`:

```text
kernel::testing::trivial_assertion ... [failed]

Error: panicked at src/testing.rs:41:5:
assertion `left == right` failed
  left: 2
 right: 3
[test] FAIL: QEMU exit status 35
```

and `make` stops with an error – exactly what a CI system needs.

## The tests of this chapter
- `trivial_assertion` – the framework itself works.
- `limine_supports_our_base_revision` – the boot protocol handshake from chapter 1.
- `long_lines_are_sent_completely` – 128 bytes in a row, more than the UART's 16-byte buffer: `send` must wait
  for the hardware instead of losing bytes.

## What can go wrong
- **`cargo test` says "running 0 tests" and passes immediately, without QEMU:** the binary has `test = false` in
  `Cargo.toml`, or the tests are marked `#[test]` instead of `#[test_case]`.
- **Test mode is never detected, the test kernel opens a window:** the file-name rule did not match. The reference
  kernel had exactly this bug when a new Cargo version moved the test binaries to another directory
  (bug #1 in the [bug log](../BUGS.md)) – which is why the rule looks at the name, not the path.
- **QEMU exits with status 1 instead of 33/35:** the `-device isa-debug-exit,…` option is missing.
- **A test hangs forever:** without the timeout in `run-qemu.sh` nothing would ever end it.

## Experiments
1. Add a test that loops forever (`loop {}`). How long until it fails, and what does the script print?
2. Call `qemu::exit(qemu::ExitCode::Success)` at the end of the *normal* `kmain` (remove the `#[cfg(test)]` in front
   of `mod qemu` first). What happens to `make run` – and to the window?
3. Make the panic in a test happen inside `serial_println!` (format something that panics in its `Display`
   implementation). Is the error still reported? Why might a panic handler that prints be dangerous?
