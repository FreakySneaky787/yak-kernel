# KernelTry – Understand the Kernel

KernelTry is a small x86-64 operating system written from scratch in Rust, for **learning how a kernel works** by
reading, running, testing and changing one. It runs in QEMU (BIOS and UEFI).

![KernelTry shell running in QEMU: uname, cpu, netinfo, ps and df](images/shell.png)

## Where to start
1. **Get it running** – the [quick start in the README](../README.md#quick-start): host packages, `make run`,
   `make check`.
2. **Follow one thing through the kernel** – the [walkthroughs](walkthrough/README.md) trace a boot, a key press, a
   pipeline and a page fault through the code, with links to every function involved.
3. **Change something and watch what happens** – the [experiments](EXPERIMENTS.md), each with a measured result.
4. **Build on it** – [adding a syscall, a program, a test](HACKING.md).

The [learning notes](LEARN.md) explain every subsystem in the order it was built; the [design notes](DESIGN.md) give
the reasons behind each decision, and the [bug log](BUGS.md) tells what went wrong and how it was found.

## The course
KernelTry is turning into a [course](course/README.md) in which you **rebuild the kernel chapter by chapter**, each
chapter a runnable, tested state of its own. The first chapters are being written; the plan is in the
[roadmap](ROADMAP.md).

## About
KernelTry is free software under the GNU General Public License v3 or later
([LICENSE](../LICENSE)), developed by FreakySneaky787 together with an AI assistant. Source code and issues:
[github.com/FreakySneaky787/yak-kernel](https://github.com/FreakySneaky787/yak-kernel).
