# 03 – `cat /README.txt | wc`

Two programs run at the same time, connected by a pipe: `cat` reads a file from the FAT32 disk and writes it to its
standard output; `wc` reads its standard input and counts. Neither program knows about the other.

**Try it:** type the line in `make run`. `ps` in a second pipeline (`cat | ps`) shows both sides as processes.

## 1. The shell parses the line
[`run_pipeline`](../../user/src/bin/sh/main.rs) gets the parsed pipeline (tokenizer and parser:
[`syntax.rs`](../../user/src/bin/sh/syntax.rs)) with two stages. It first saves its own descriptors 0, 1, 2 with `dup`,
then for every stage:

- **stdin** is the read end of the previous pipe (or the shell's own stdin for the first stage),
- **stdout** is the write end of a new pipe (or the shell's own stdout for the last stage),

set with `dup2` and followed by `close` of the original descriptor. Then the stage is started with `spawn`. The child
**inherits descriptors 0–2** – this is the whole trick of Unix pipelines: the program just uses 0 and 1.

## 2. `pipe`, `dup2`, `spawn` in the kernel
- [`sys_pipe`](../../kernel/src/process/syscall.rs) calls [`pipe`](../../kernel/src/process/pipe.rs): one 64 KiB queue,
  two [`PipeEnd`](../../kernel/src/process/pipe.rs)s. Each end counts as a reader or writer; cloning an end (dup,
  inheritance) increments the count, dropping it ([`Drop`](../../kernel/src/process/pipe.rs)) decrements it.
- [`sys_dup2`](../../kernel/src/process/syscall.rs) copies a handle into another slot of the process's descriptor table.
- [`sys_spawn`](../../kernel/src/process/syscall.rs) copies path and arguments from user memory, reads the program from
  the disk, takes descriptors 0–2, cwd, environment and signal settings of the parent, and creates the process with
  [`spawn_with`](../../kernel/src/process/mod.rs) (see walkthrough 01). Both children now exist and are scheduled
  independently – on different CPUs if there are several.

When the shell has started both, it restores its own descriptors and closes its copies of the pipe ends. **Now only
`cat` holds the write end.** That matters for step 5.

## 3. `cat` reads the file
`cat` opens `/README.txt` ([`sys_open`](../../kernel/src/process/syscall.rs)) and calls `read` in a loop.
[`sys_read`](../../kernel/src/process/syscall.rs) finds a file handle and asks the file system:
[`read_at`](../../kernel/src/fs/fat32.rs) looks up the path (a cached directory walk,
[`lookup`](../../kernel/src/fs/fat32.rs)), gets the file's cluster chain from the FAT, and reads runs of consecutive
clusters with one request through the sector cache ([`BlockCache`](../../kernel/src/drivers/block.rs)) and the AHCI
driver ([`ahci.rs`](../../kernel/src/drivers/ahci.rs)). While the disk works, the thread blocks and the CPU runs others
(the completion arrives as an MSI interrupt, M32).

## 4. Through the pipe
`cat` writes the data to descriptor 1. [`sys_write`](../../kernel/src/process/syscall.rs) sees a pipe end and calls
[`write`](../../kernel/src/process/pipe.rs): as many bytes as fit go into the queue, then
[`notify`](../../kernel/src/task/mod.rs)`(Wait::Pipe(id))` wakes a waiting reader. If the queue is full, the writer
blocks until the reader made room – this is **flow control**: a fast writer cannot use up memory.

`wc` reads descriptor 0, which is the read end: [`read`](../../kernel/src/process/pipe.rs) takes what is there, or
blocks with [`wait_for`](../../kernel/src/task/mod.rs) while the queue is empty *and* writers exist.

## 5. End of file – the subtle part
`cat` reaches the end of the file and exits. [`exit_current`](../../kernel/src/process/mod.rs) clears its descriptor
table; the write end is dropped, the writer count becomes 0, and the readers are woken. `wc`'s next `read` finds an
empty queue with no writer and returns 0 bytes: **end of file**. `wc` prints its counts and exits too.

If the shell had kept a copy of the write end, `wc` would wait forever – a classic pipeline bug, and the reason the
shell closes every pipe end it does not need.

## 6. The shell collects the results
The shell waits for every child ([`wait_all`](../../user/src/bin/sh/main.rs) → `wait`). In the kernel
[`wait_child`](../../kernel/src/process/mod.rs) checks that the pid is really a child, lends the console (for Ctrl+C) to
it while waiting, blocks until the child's thread has ended, removes the process from the table and returns its exit
status. The status of the **last** stage becomes `$?`.

## Things to try
- `cat /README.txt | wc` vs. `wc /README.txt`: the same numbers, but one goes through a pipe.
- `pipetest` writes 200 KB through a pipe – three times the pipe's capacity – and checks every byte.
- Shrink [`PIPE_CAPACITY`](../../kernel/src/process/pipe.rs) – experiment 4 in [`EXPERIMENTS.md`](../EXPERIMENTS.md).
