# 02 – A key press reaches a program

You press `ä` on the keyboard while `/bin/sh` waits for input. What happens until the character appears in the line
editor?

**Try it:** `python3 -I scripts/qemu-monitor.py kernel/target/x86_64-unknown-none/debug/kernel 5 -- "info pic" "info lapic"`
shows the interrupt controllers; `make debug` + `make gdb` with `break kernel::arch::x86_64::irq::keyboard` stops in the handler.

## 1. The program goes to sleep
The shell's [`read_line`](../../user/src/bin/sh/main.rs) calls `sys::read(STDIN, …)`. The `syscall` instruction lands in
the assembly stub `syscall_entry` ([`syscall.rs`](../../kernel/src/arch/x86_64/syscall.rs)), which switches to the
thread's kernel stack, saves the registers as a [`SyscallFrame`](../../kernel/src/arch/x86_64/syscall.rs) and calls
[`dispatch`](../../kernel/src/process/syscall.rs). For descriptor 0 [`sys_read`](../../kernel/src/process/syscall.rs)
goes to [`read_stdin`](../../kernel/src/process/syscall.rs), which calls
[`read_key_or_abort`](../../kernel/src/input.rs).

No key is there yet, so the thread blocks: [`wait_for`](../../kernel/src/task/mod.rs)`(Wait::Input, …)` marks it
*blocked on input* **first** and only then checks the condition again. If a key arrives on another CPU between the
check and the switch, the wake-up is not lost (see M26 in `docs/LEARN.md`). The scheduler runs another thread – or the
idle thread, which halts the CPU with `hlt`.

## 2. The hardware interrupt
The keyboard controller puts a **scancode** into its data port and raises IRQ 1. The I/O APIC was told in
[`irq::init`](../../kernel/src/arch/x86_64/irq.rs) to deliver it as vector 33 to the boot CPU; the CPU looks up the
IDT entry and runs [`keyboard`](../../kernel/src/arch/x86_64/irq.rs) (an `extern "x86-interrupt"` function):

1. [`handle_interrupt`](../../kernel/src/drivers/keyboard.rs) reads the byte from port 0x60. A tiny tracker of the
   Ctrl key ([`track_ctrl_c`](../../kernel/src/drivers/keyboard.rs)) recognises Ctrl+C right here and sends SIGINT to the
   foreground process ([`interrupt_foreground`](../../kernel/src/process/mod.rs)) – that is why Ctrl+C works even while
   a program never reads. Every other byte goes into a ring buffer ([`RingBuffer`](../../kernel/src/ringbuf.rs)).
   *Translating* the scancode is not done here: interrupt handlers must be short.
2. [`wake_input_waiters`](../../kernel/src/task/mod.rs) → [`notify`](../../kernel/src/task/mod.rs) marks every thread
   blocked on `Wait::Input` as ready.
3. [`eoi`](../../kernel/src/arch/x86_64/lapic.rs) tells the local APIC the interrupt is handled.
4. [`preempt_idle`](../../kernel/src/task/mod.rs): if this CPU was idle, switch to the woken thread now instead of at
   the next timer tick (bug #7 in `docs/BUGS.md`).

`ä` on a German keyboard is one scancode (0x28); a key with Shift or AltGr is several bytes, and every key also sends a
*release* code (pressed code + 0x80).

## 3. Back in the woken thread
The shell's thread continues inside `read_key_or_abort`. [`try_read_key`](../../kernel/src/input.rs) takes the bytes
from the ring buffer and feeds them to the scancode [`Decoder`](../../kernel/src/drivers/scancode.rs): it follows Shift,
Caps Lock, Ctrl, AltGr and the `E0` prefix of extended keys, ignores releases, and maps the key position through the
layout table (German by default) to a [`KeyEvent`](../../kernel/src/shell/keys.rs) – here `Key::Char('ä')`.

The serial port takes a parallel path: [`com1`](../../kernel/src/arch/x86_64/irq.rs) stores the byte, and
[`TerminalDecoder`](../../kernel/src/input.rs) turns UTF-8 and VT100 escape sequences into the same `KeyEvent`s. That is
how `make e2e` can type into the shell.

## 4. Into the program's buffer
A program reads bytes, not key events, so [`key_bytes`](../../kernel/src/process/syscall.rs) turns the event into what a
terminal would send: `ä` becomes the two UTF-8 bytes `C3 A4`, arrow keys become `ESC [ D` and so on. Bytes that do not
fit into the program's buffer wait in `stdin_pending` for the next `read`.
[`copy_to_user`](../../kernel/src/process/syscall.rs) checks the buffer and copies under the process table lock (bug #14),
`dispatch` puts the byte count into RAX and `sysretq` returns to ring 3.

## 5. In the shell
`read_line` feeds the bytes to its own [`Decoder`](../../user/src/bin/sh/keys.rs) (the user-space twin of the
kernel's terminal decoder) and hands the `KeyEvent` to the line editor
([`handle`](../../kernel/src/shell/line_editor.rs) – the same source file is compiled into the kernel shell and `/bin/sh`).
The editor inserts `ä` and writes it back with a `write` syscall – that is the echo you see.

## Things to try
- `layout us` (a kernel command, reached through `kshell`) switches the table in
  [`scancode.rs`](../../kernel/src/drivers/scancode.rs): type `y`, `z` and `ä` before and after.
- Type while `fault spin` runs (it never reads): the bytes wait in the ring buffer and appear in the shell afterwards –
  except Ctrl+C, which the interrupt handler turns into SIGINT at once.
