# 01 – From power-on to `/bin/sh`

**Try it:** `make run` and watch the serial output in the terminal; `make debug` + `make gdb` and `break kmain` to step
through it.

## 1. Firmware and Limine
The firmware (BIOS or UEFI/OVMF) starts Limine from the ISO that [`run-qemu.sh`](../../scripts/run-qemu.sh) builds.
Limine reads [`limine.conf`](../../limine.conf), loads the kernel ELF to the higher half (`0xffffffff80000000`, see
[`linker.ld`](../../kernel/linker.ld)), switches to 64-bit long mode with paging on, and fills in the answers to our
**requests** – static structs in [`boot.rs`](../../kernel/src/boot.rs) such as [`BASE_REVISION`](../../kernel/src/boot.rs)
(memory map, framebuffer, HHDM offset, RSDP). Then it jumps to [`kmain`](../../kernel/src/main.rs).

At this point interrupts are off, we run on Limine's stack and Limine's page tables.

## 2. `kernel::init` – one subsystem after another
[`init`](../../kernel/src/lib.rs) brings the machine up in an order where each step only needs the ones before it:

1. [`serial::init`](../../kernel/src/serial.rs) – COM1, so that everything after it can print.
2. The Limine protocol check ([`BASE_REVISION`](../../kernel/src/boot.rs)) before any response is read.
3. [`gdt::init`](../../kernel/src/arch/x86_64/gdt.rs) – our own segments and the TSS (with the double fault stack),
   [`fpu::init`](../../kernel/src/arch/x86_64/fpu.rs), [`enable_protection`](../../kernel/src/arch/x86_64/cpu.rs) (SMEP/SMAP),
   [`interrupts::init`](../../kernel/src/arch/x86_64/interrupts.rs) – the IDT with all exception handlers.
4. [`mem::init`](../../kernel/src/mem/mod.rs) – frame allocator from the memory map ([`pmm.rs`](../../kernel/src/mem/pmm.rs)),
   our page table code ([`paging.rs`](../../kernel/src/mem/paging.rs)), the kernel heap ([`heap.rs`](../../kernel/src/mem/heap.rs)).
   From here on `alloc` (`Vec`, `Box`, …) works.
5. [`syscall::init`](../../kernel/src/arch/x86_64/syscall.rs) – the MSRs for `syscall`/`sysret`.
6. [`irq::init`](../../kernel/src/arch/x86_64/irq.rs) – ACPI tables, local APIC timer, I/O APIC routing for keyboard and COM1.
7. [`console::init`](../../kernel/src/console.rs) – text on the framebuffer.
8. [`task::init`](../../kernel/src/task/mod.rs) – the scheduler; the code running now becomes thread 0.
9. [`init_storage`](../../kernel/src/drivers/mod.rs) – PCI scan, AHCI disk; [`mount_root`](../../kernel/src/fs/mod.rs) – FAT32.
10. [`enable_interrupts`](../../kernel/src/arch/x86_64/mod.rs) – from now on the timer preempts threads.
11. [`smp::init`](../../kernel/src/arch/x86_64/smp.rs) – wakes the other cores (INIT/SIPI, trampoline); they join the scheduler.
12. [`net::init`](../../kernel/src/net/mod.rs) – e1000, DHCP.

## 3. Our own stack, then give memory back
Back in [`kmain`](../../kernel/src/main.rs), the kernel allocates a stack with a guard page
([`alloc_stack`](../../kernel/src/mem/stack.rs)) and switches to it ([`switch_stack`](../../kernel/src/arch/x86_64/mod.rs)) –
Limine's stack has no guard page, an overflow there would silently corrupt memory. On the new stack,
[`kernel_main`](../../kernel/src/main.rs) calls [`reclaim_bootloader_memory`](../../kernel/src/mem/mod.rs): page tables
that still live in bootloader memory are copied, every CPU switches to the copy, and the old regions go back to the
frame allocator (≈ 13 MiB).

## 4. The first process
[`run_init`](../../kernel/src/process/mod.rs) reads `/bin/sh` from the disk and calls
[`spawn_with`](../../kernel/src/process/mod.rs):
- [`elf::parse`](../../kernel/src/process/elf.rs) checks the ELF header and collects the loadable segments;
- [`AddressSpace::new`](../../kernel/src/mem/address_space.rs) creates a page table whose upper half is the kernel's;
- [`elf::load`](../../kernel/src/process/elf.rs) maps each segment with the right permissions (code not writable, data not executable);
- 4 stack pages below [`USER_STACK_TOP`](../../kernel/src/process/mod.rs) are mapped, the argument block is copied to the top;
- [`spawn_user`](../../kernel/src/task/mod.rs) creates a kernel thread that will leave for ring 3, and the
  [`Process`](../../kernel/src/process/mod.rs) entry (files, cwd, environment, heap) is added to the table.

`run_init` then waits for the shell to end – if it ever does, the kernel shell ([`Shell`](../../kernel/src/shell/mod.rs)) takes over.

## 5. Into ring 3
The scheduler picks the new thread. It starts in [`thread_start`](../../kernel/src/task/mod.rs), which sees the
`user_entry` and calls [`enter_user`](../../kernel/src/arch/x86_64/syscall.rs). The assembly `enter_user_mode` pushes a
fake interrupt frame (user SS, RSP, RFLAGS with IF, user CS, RIP), clears every register so no kernel value leaks, and
executes `iretq` – the CPU is now in ring 3 at the ELF entry point.

That entry point is `_start` in [`ulib`](../../user/src/lib.rs): it aligns the stack and calls the program's `main`
(declared with [`entry!`](../../user/src/lib.rs)). The shell prints its prompt with a `write` syscall – the first time a
user program asks the kernel for something.

## Things to try
- `make debug`, `make gdb`, `break kernel::process::spawn_with`, `continue`: inspect the ELF segments being mapped.
- `mv /bin/sh /bin/sh.off; exit` in the running system: `run_init` returns and the kernel shell starts
  (`kernel:/>`); after a reboot, `run_init` cannot find `/bin/sh` at all. `mv /bin/sh.off /bin/sh` in the kernel shell
  puts it back.
