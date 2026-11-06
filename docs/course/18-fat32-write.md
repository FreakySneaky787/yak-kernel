# 18 – Writing FAT32

**Goal:** create, change, append to and remove files and directories on the FAT32 volume – so that afterwards
`fsck.fat` finds nothing wrong and the Linux tools read exactly what the kernel wrote.
**You end up with:** WRITE DMA EXT and FLUSH in the AHCI driver; free clusters counted and allocated; `write_file`,
`append`, `create_dir` and `remove`; `write`, `append`, `mkdir` and `rm` in the shell; and a `make test` that hands the
disk to `fsck.fat` and mtools after QEMU has ended.
**Code:** [`course/18-fat32-write/`](https://github.com/FreakySneaky787/yak-kernel/tree/main/course/18-fat32-write)
(chapter 17 plus writing in `ahci.rs`, `block.rs`, `fs.rs` and `fat32.rs`; `check-disk.sh`)
**What changed:** `diff -ru course/17-fat32 course/18-fat32-write` (in the repository) shows every line this chapter
adds.

## Background: one change is several writes
Writing a new file touches four places: the clusters with the data, their entries in the FAT, the same entries in
the second FAT, and an entry in the directory. Each is one write to the disk, and the machine can stop between any
two of them. A file system cannot make that impossible – but it can choose the *order*, so that whatever has been
written so far leaves a volume that is still consistent, at worst with clusters nobody uses:

1. data into free clusters,
2. the chain in the FAT (all copies),
3. the directory entry that points to it,
4. the free count in the FSInfo sector, and a FLUSH, so that the disk's own cache is empty too.

Stopped after 1 or 2: the clusters are lost – used in the FAT, but by no file; `fsck.fat` gives them back. Stopped
after 3: everything is there. The reverse order would be worse: an entry pointing to clusters that are still free
and may be given to another file. Replacing a file follows the same rule – the new data, then the entry, *then* the
old clusters are freed.

How well this holds against real power loss is a topic of its own. The reference kernel tests it with a disk that
fails on purpose and tears a write in half (`FaultyDisk` in
[`kernel/src/drivers/block.rs`](https://github.com/FreakySneaky787/yak-kernel/blob/main/kernel/src/drivers/block.rs),
milestone M53): without a journal, FAT32 can still be left with lost clusters or a chain longer than its file –
`fsck.fat` repairs both. This chapter keeps to the order and checks the result with `fsck.fat`.

## Step 1: writing sectors
The AHCI command of chapter 16 learns the other direction. Bit 6 of the command header ("W") says the data goes to
the disk:

```rust,file=kernel/src/ahci.rs
            let direction = if to_disk { 1 << 6 } else { 0 };
            write_volatile(header, 5 | direction | prds << 16);
```

`write` copies each piece of up to 8 sectors into the bounce buffer and sends WRITE DMA EXT (0x35); `flush` sends
FLUSH CACHE EXT (0xEA), which returns when the disk has its cache on the medium. `BlockDevice` gets both as methods,
and `fs::SystemDisk` passes them on to `ahci::DISK`.

## Step 2: free clusters
At mount, `count_free` reads the whole FAT – 8 sectors per command – and counts the entries that are 0. The FSInfo
sector holds such a count too, but the specification calls it a hint: after a crash it may be wrong, so we count
ourselves and only *write* it. On this chapter's test disk the count is 128 769 – 129 022 clusters minus the
253 that `fsck.fat` reports as used.

A FAT entry is changed in every copy of the FAT, keeping the 4 reserved bits:

```rust,file=kernel/src/fat32.rs
    fn set(&mut self, cluster: u32, value: u32) -> Result<()> {
        let offset = cluster as u64 * 4;
        let sector = offset / SECTOR_SIZE as u64;
        let at = (offset % SECTOR_SIZE as u64) as usize;
        let mut data = [0u8; SECTOR_SIZE];
        self.disk.read(self.fat_start + sector, &mut data)?;
        let old = u32_at(&data, at);
        put_u32(&mut data, at, (old & !FAT_MASK) | (value & FAT_MASK));
        for copy in 0..self.fats {
            self.disk.write(self.fat_start + copy * self.fat_size + sector, &data)?;
        }
        Ok(())
    }
```

`allocate` refuses at once when the count is too small – before anything is written – and otherwise searches from
`next_free` for entries that are 0. It links the clusters it found back to front, so the chain is complete before
anything points to it, and only then hangs it onto `after` (the last cluster of a file that grows):

```rust,file=kernel/src/fat32.rs
        for (i, &c) in found.iter().enumerate().rev() {
            self.set(c, found.get(i + 1).copied().unwrap_or(END_MARK))?;
        }
        if let Some(last) = after {
            self.set(last, found[0])?;
        }
```

## Step 3: directory entries
`entries` (chapter 17) now also remembers where each entry is: its `slots` – the long name pieces and the 8.3 entry
– and its 8.3 name as stored. `free_slot` finds a deleted or unused entry in a directory; when every cluster is full,
the directory grows by one cluster of zeros (all "end of directory").

Names: this chapter writes 8.3 names only. `short_name_for` turns "notes.txt" into `NOTES   TXT` with both
lower-case flags; a name that needs more – longer than 8 + 3, mixed case like "ReadMe", spaces – is `BadName`.
(Writing long names means creating a unique alias like `ALONGF~1` and the pieces with their checksum: an
experiment below.) One trap: a file with a long name *has* an 8.3 name too. `prepare` refuses a new name that is
the alias of an existing file, or the directory would hold the same 8.3 name twice.

## Step 4: the operations
Each public operation ends with `sync`: the free count into the FSInfo sector, then FLUSH. When it returns, the
change is on the disk.

```rust,file=kernel/src/fat32.rs
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let size = u32::try_from(data.len()).map_err(|_| FsError::NoSpace)?;
        match self.lookup(path) {
            Ok(old) if old.is_dir => return Err(FsError::IsADirectory),
            Ok(old) => {
                // new data first, then the entry points to it, then the old clusters are freed
                let first = self.store(data, None)?;
                let slot = *old.slots.last().expect("a file has an entry");
                self.write_slot(slot, |e| {
                    put_u16(e, 20, (first >> 16) as u16);
                    put_u16(e, 26, first as u16);
                    put_u32(e, 28, size);
                })?;
                self.free_chain(old.cluster)?;
            }
            Err(FsError::NotFound) => {
                let (raw, case, dir) = self.prepare(path)?;
                let first = self.store(data, None)?;
                let slot = self.free_slot(dir)?;
                self.write_slot(slot, |e| {
                    e.copy_from_slice(&entry_bytes(&raw, case, ATTR_ARCHIVE, first, size))
                })?;
            }
            Err(other) => return Err(other),
        }
        self.sync()
    }
```

`append` fills the free end of the file's last cluster first and hangs new clusters behind it for the rest.
`create_dir` writes a cluster with "." (the directory itself) and ".." (its parent – 0 when that is the root, as
the specification wants) before the entry that makes it visible. `remove` deletes the entry first, then frees the
clusters – and deletes *every* slot of the entry, the long name pieces included:

```rust,file=kernel/src/fat32.rs
    pub fn remove(&mut self, path: &str) -> Result<()> {
        split(path)?; // not "/"
        let entry = self.lookup(path)?;
        if entry.is_dir && !self.entries(entry.cluster)?.is_empty() {
            return Err(FsError::NotEmpty);
        }
        for &slot in &entry.slots {
            self.write_slot(slot, |e| e[0] = ENTRY_DELETED)?;
        }
        self.free_chain(entry.cluster)?;
        self.sync()
    }
```

Dates: the kernel has no clock that knows the date yet, so every entry says 1980-01-01, the first day FAT can
store.

## Step 5: the shell, and checking from the outside
`write <file> [words]` makes a file of the words (and a newline), `append` adds them, `mkdir` and `rm` do what they
say. `mkdisk.sh` adds a directory `trash/` with one long-named file for the removal test.

The strongest test is not inside the kernel. `check-disk.sh` runs after QEMU has ended, on the disk image the kernel
wrote: `fsck.fat -n` (look, change nothing) must find no problem, and `mtype` must return byte for byte what the
kernel wrote:

```sh,file=check-disk.sh
if ! fsck.fat -n "$IMG" > build/fsck.log 2>&1; then
```

`make test` runs it twice: after the unit tests (which leave `/kept.txt` and a 100 000-byte `/keptdir/big.bin` for
it), and after the boot check, which types `write hi.txt hello disk`, `append hi.txt and more` and `cat hi.txt`.

## The tests
`make test` runs 132 tests in the kernel; new:
- `a_new_file_is_written_and_read_back` (also by a second, fresh mount), `replacing_a_file_gives_its_clusters_back`
  (6 clusters → 1 → none, the free count follows), `append_fills_the_last_cluster_then_grows`;
- `directories_are_made_and_grow` (20 files: the directory grows to 2 clusters; a directory in it),
  `removing_frees_clusters_and_the_name` (also a long-named file made by mtools; a non-empty directory is refused);
- `names_and_paths_are_checked` (long, mixed-case and too long names, missing parents, the hidden alias),
  `a_full_volume_is_refused_before_anything_is_written`, `the_free_count_stays_exact` (the running count equals a
  fresh count of the FAT);
- `write_append_mkdir_and_rm` (the shell), and `files_for_the_host_to_check`.

Then `check-disk.sh`: `fsck.fat` clean, and mtools reads `/kept.txt`, `/keptdir/big.bin` and `/hi.txt` as written.

Three counter-checks, each run once, each caught by `check-disk.sh`:
- `set` writing only the first FAT: "FATs differ but appear to be intact."
- `remove` deleting only the 8.3 entry: "Orphaned long file name part "to be removed.txt"".
- The writing tests each mounting a `FatFs` of their own (my first version): "Free cluster summary wrong (128769 vs.
  really 128542)". Two mounts of one disk each keep their own free count; whichever writes the FSInfo sector last
  writes a wrong one. The tests now use the volume the kernel mounted at boot.

## Run it
```sh
cd course/18-fat32-write
make run
```
and in the shell:

```text
yak> mkdir notes
yak> write notes/todo.txt buy milk
yak> append notes/todo.txt write chapter 19
yak> cat notes/todo.txt
buy milk
write chapter 19
yak> rm notes
rm: /notes: NotEmpty
```

After QEMU has ended, the host reads the same: `MTOOLS_SKIP_CHECK=1 mtype -i build/disk.img ::/notes/todo.txt`.

## What can go wrong
- **The wrong order:** an entry written before its clusters are linked points into free space; a later file gets the
  same clusters, and two files share data. The order of step 4 avoids it.
- **One FAT only:** other systems compare the copies – `fsck.fat` noticed at once.
- **Long name pieces left behind:** they belong to no entry any more; `fsck.fat` calls them orphaned.
- **Two mounts of one disk:** each believes its own free count – and its own idea of what is free. Here it only
  made the FSInfo count wrong (allocation always reads the FAT itself); a kernel that kept a free-cluster bitmap in
  memory would hand the same cluster to two files.
- **Tests that depend on each other:** the tests do not run in the order of the file but sorted by their names
  (that is how the compiler hands them to the runner). My first removal
  test deleted a file that a read test of chapter 17 expected – and a test that compared the whole root directory
  saw the files of the writing tests. Each test now has its own files, and the read test checks only what
  `mkdisk.sh` made.
- **A directory that grows:** a new entry may need a cluster for the directory itself; a test that expects the free
  count back after write + remove has to count after the write.

## Experiments
1. Stop QEMU (`quit` in its monitor) between `store` and `write_slot` – e.g. with a `sleep` in between – and run
   `fsck.fat -n` on the image. What does it report, and why is the file system still usable?
2. Write long names: generate `BASIS~N.EXT` aliases (counting N up until no entry has it), the pieces with the
   checksum of chapter 17, and write them in front of the 8.3 entry.
3. `count_free` reads 1009 FAT sectors at mount. How long does it take? Would trusting the FSInfo count be worth the
   risk?
4. Let `write_file` fail half way on purpose (return an error after `store`). Which clusters are lost, and what does
   `fsck.fat -n` say about them?

Next, chapter 19 makes the disk faster and the CPU free: a block cache, and the disk's interrupt instead of polling.
