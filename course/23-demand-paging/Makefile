# Chapter 23 - User heap and demand paging.
#   KERNEL_SMP=1 make test (or 2, ...) runs it with another number of CPUs; 4 if not set
#   make programs  build the user programs (programs/*.S) with `as` and `ld` into build/programs/; mkdisk.sh puts
#               them into /bin of the test disk
#   make run    build the programs and the kernel and boot it in a QEMU window (serial output in this terminal), with
#               a 64 MiB FAT32 test disk (build/disk.img, made by mkdisk.sh with mkfs.fat and mtools);
#               KERNEL_CMDLINE=fault=page make run (or fault=stack, fault=overflow, fault=heap) crashes it on purpose
#   make test   run the unit tests inside the kernel (`cargo test`) and check the disk they wrote with fsck.fat
#               and mtools (check-disk.sh), then boot the normal kernel, type eleven commands into its shell,
#               check their output and the disk again - and boot it twice more with a fault on purpose,
#               which the handlers must report, once more with a real stack overflow into a guard page, and once
#               more asking the heap for more than it may map
LIMINE_DIR ?= $(CURDIR)/vendor/limine
LIMINE_BRANCH := v11.x-binary
KERNEL := kernel/target/x86_64-unknown-none/debug/kernel
export LIMINE_DIR
# what the check types (QEMU key names = US key positions), one command per line:
#   echo Hi  YAK!    with Shift and Caps Lock - two spaces, but echo prints one
#   echo shelx<-l    a typo erased with Backspace
#   nope             no such command
#   mem
#   threads
#   write hi.txt hello disk
#   append hi.txt and more
#   cat hi.txt
#   run /bin/hello   a program from the disk, in ring 3
#   run /bin/copy    a program that copies /hello.txt to /copy.txt
#   run /bin/heap    a program with a heap of 32 MiB of which it touches three pages
TYPED := e c h o spc shift-h i spc spc caps_lock y a k caps_lock shift-1 ret \
	e c h o spc s h e l x backspace l ret \
	n o p e ret \
	m e m ret \
	t h r e a d s ret \
	w r i t e spc h i dot t x t spc h e l l o spc d i s k ret \
	a p p e n d spc h i dot t x t spc a n d spc m o r e ret \
	c a t spc h i dot t x t ret \
	r u n spc slash b i n slash h e l l o ret \
	r u n spc slash b i n slash c o p y ret \
	r u n spc slash b i n slash h e a p ret

# The user programs (chapter 21): each programs/NAME.S becomes the ELF file build/programs/NAME. `as` assembles,
# `ld` links with user.ld (code at 0x400000, data at 0x401000); max-page-size keeps the segments one page apart.
PROGRAMS := $(patsubst programs/%.S,build/programs/%,$(wildcard programs/*.S))

.PHONY: run test clean programs

programs: $(PROGRAMS)

build/programs/%: programs/%.S programs/user.ld
	mkdir -p build/programs build/objects
	as -o build/objects/$*.o $<
	ld -z max-page-size=0x1000 -T programs/user.ld -o $@ build/objects/$*.o

run: $(LIMINE_DIR)/limine-bios-cd.bin programs
	cd kernel && cargo run

test: $(LIMINE_DIR)/limine-bios-cd.bin programs
	cd kernel && cargo test
	./check-disk.sh tests
	cd kernel && cargo build
	KERNEL_MODE=check KEYS_AFTER="Shell: type help" KEYS="$(TYPED)" ./run-qemu.sh $(KERNEL)
	./check-disk.sh shell
	KERNEL_MODE=check KERNEL_CMDLINE=fault=page EXPECTED=expected-page-fault.txt ./run-qemu.sh $(KERNEL)
	KERNEL_MODE=check KERNEL_CMDLINE=fault=stack EXPECTED=expected-double-fault.txt ./run-qemu.sh $(KERNEL)
	KERNEL_MODE=check KERNEL_CMDLINE=fault=overflow EXPECTED=expected-overflow.txt ./run-qemu.sh $(KERNEL)
	KERNEL_MODE=check KERNEL_CMDLINE=fault=heap EXPECTED=expected-heap.txt ./run-qemu.sh $(KERNEL)

# The Limine bootloader is downloaded, not part of our code
$(LIMINE_DIR)/limine-bios-cd.bin:
	git clone --depth=1 --branch=$(LIMINE_BRANCH) https://github.com/limine-bootloader/limine.git $(LIMINE_DIR)

clean:
	rm -rf build kernel/target
