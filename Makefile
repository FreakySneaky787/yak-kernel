# KernelTry - entry point for all tasks. See CLAUDE.md.
SHELL      := /bin/bash
CARGO      := source $$HOME/.cargo/env && cd kernel && cargo
LIMINE_DIR := vendor/limine
LIMINE_BRANCH := v11.x-binary
KERNEL_ELF := kernel/target/x86_64-unknown-none/debug/kernel
# User programs (user/src/bin/<name>.rs or user/src/bin/<name>/main.rs) are copied to /bin on every disk image
USER_PROGRAMS := $(basename $(notdir $(wildcard user/src/bin/*.rs))) \
                 $(notdir $(patsubst %/,%,$(dir $(wildcard user/src/bin/*/main.rs))))
USER_OUT   := user/target/x86_64-kerneltry-user/release
USER_BIN   := build/user/bin
# UEFI firmware: Fedora (edk2-ovmf) or Debian/Ubuntu (ovmf) path; override with `make OVMF=<file>`
OVMF       ?= $(firstword $(wildcard /usr/share/edk2/ovmf/OVMF_CODE.fd /usr/share/OVMF/OVMF_CODE_4M.fd \
                  /usr/share/OVMF/OVMF_CODE.fd) /usr/share/edk2/ovmf/OVMF_CODE.fd)

.PHONY: all limine limine-tool user build run run-uefi debug gdb test e2e e2e-uefi check fmt \
        disk disk-reset disasm symbols sections addr2line clean distclean

all: build

limine: $(LIMINE_DIR)/limine-bios-cd.bin
$(LIMINE_DIR)/limine-bios-cd.bin:
	git clone --depth=1 --branch=$(LIMINE_BRANCH) https://github.com/limine-bootloader/limine.git $(LIMINE_DIR)

# Host utility for `limine bios-install` (makes the ISO bootable as a hard disk / USB image).
# Optional: needs gcc. run-qemu.sh uses it automatically when it exists.
limine-tool: limine
	$(MAKE) -C $(LIMINE_DIR) CC=gcc

# User programs -> build/user/bin (scripts/mkdisk.sh puts them into /bin)
user:
	source $$HOME/.cargo/env && cd user && cargo build --release
	mkdir -p $(USER_BIN)
	cp $(addprefix $(USER_OUT)/,$(USER_PROGRAMS)) $(USER_BIN)/

build: limine user
	$(CARGO) build

# Window + serial output in the terminal
run: limine user
	$(CARGO) run

run-uefi: build
	scripts/run-qemu.sh $(KERNEL_ELF) -drive if=pflash,format=raw,readonly=on,file=$(OVMF)

# QEMU waits for gdb; in a second terminal run: make gdb
debug: build
	KERNEL_DEBUG=1 scripts/run-qemu.sh $(KERNEL_ELF)

gdb:
	gdb -q $(KERNEL_ELF) -ex 'set disassembly-flavor intel' -ex 'target remote :1234'

# All in-kernel tests in QEMU, exit status 0 = all green
test: limine user
	rm -rf build/test-disks
	$(CARGO) test
	scripts/check-test-disks.sh

# End-to-end: boot the real kernel and drive the shell over the serial port
e2e: build
	python3 -I scripts/e2e-shell.py $(KERNEL_ELF)

# Same, but booted through UEFI firmware (OVMF)
e2e-uefi: build
	python3 -I scripts/e2e-shell.py $(KERNEL_ELF) -drive if=pflash,format=raw,readonly=on,file=$(OVMF)

# Everything: unit + integration tests, then the end-to-end shell test (BIOS and UEFI)
check: test e2e e2e-uefi

# Format all kernel code (rustfmt, see kernel/rustfmt.toml)
fmt:
	$(CARGO) fmt
	source $$HOME/.cargo/env && cd user && cargo fmt

# Persistent FAT32 disk used by `make run` (created from rootfs/ if missing)
DISK_IMG := build/disk.img
disk: $(DISK_IMG)
$(DISK_IMG):
	scripts/mkdisk.sh $(DISK_IMG) 64

# Throw away all changes made on the disk and recreate it from rootfs/
disk-reset:
	scripts/mkdisk.sh $(DISK_IMG) 64

# ---- GNU binutils helpers ----------------------------------------------------

# Disassembly with source interleaved (Intel syntax), paged through less
disasm: build
	objdump -d -S -C -M intel --no-show-raw-insn $(KERNEL_ELF) | less

# All symbols sorted by address
symbols: build
	nm -C -n $(KERNEL_ELF)

# Program headers (what Limine loads) and sections
sections: build
	readelf -lSW $(KERNEL_ELF)

# Map an address (e.g. RIP from a panic message) to function + file:line
#   make addr2line ADDR=0xffffffff80004d9f
addr2line: build
	@test -n "$(ADDR)" || { echo "usage: make addr2line ADDR=0x..."; exit 1; }
	addr2line -f -C -i -e $(KERNEL_ELF) $(ADDR)

clean:
	$(CARGO) clean
	source $$HOME/.cargo/env && cd user && cargo clean
	rm -rf build

distclean: clean
	rm -rf vendor
