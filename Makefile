# KernelTry – Einstiegspunkt für alle Aufgaben. Siehe CLAUDE.md.
SHELL      := /bin/bash
CARGO      := source $$HOME/.cargo/env && cd kernel && cargo
LIMINE_DIR := vendor/limine
LIMINE_BRANCH := v11.x-binary
KERNEL_ELF := kernel/target/x86_64-unknown-none/debug/kernel
OVMF       := /usr/share/edk2/ovmf/OVMF_CODE.fd

.PHONY: all limine build run run-uefi debug gdb test clean distclean

all: build

limine: $(LIMINE_DIR)/limine-bios-cd.bin
$(LIMINE_DIR)/limine-bios-cd.bin:
	git clone --depth=1 --branch=$(LIMINE_BRANCH) https://github.com/limine-bootloader/limine.git $(LIMINE_DIR)

build: limine
	$(CARGO) build

# Fenster + Serial-Ausgabe im Terminal
run: limine
	$(CARGO) run

run-uefi: build
	scripts/run-qemu.sh $(KERNEL_ELF) -drive if=pflash,format=raw,readonly=on,file=$(OVMF)

# QEMU wartet auf gdb; in zweitem Terminal: make gdb
debug: build
	KERNEL_DEBUG=1 scripts/run-qemu.sh $(KERNEL_ELF)

gdb:
	gdb -q $(KERNEL_ELF) -ex 'target remote :1234'

# Alle In-Kernel-Tests in QEMU, Exitcode 0 = alles grün
test: limine
	$(CARGO) test

clean:
	$(CARGO) clean
	rm -rf build

distclean: clean
	rm -rf vendor
