# Chapter 1 - Booting with Limine.
#   make run    build the kernel and boot it in a QEMU window (serial output in this terminal)
#   make test   boot it headless and check what it prints on the serial port
LIMINE_DIR ?= vendor/limine
LIMINE_BRANCH := v11.x-binary
KERNEL := kernel/target/x86_64-unknown-none/debug/kernel

.PHONY: build run test clean

build: $(LIMINE_DIR)/limine-bios-cd.bin
	cd kernel && cargo build

run: build
	LIMINE_DIR=$(LIMINE_DIR) ./run-qemu.sh $(KERNEL) run

test: build
	LIMINE_DIR=$(LIMINE_DIR) ./run-qemu.sh $(KERNEL) test

# The Limine bootloader is downloaded, not part of our code
$(LIMINE_DIR)/limine-bios-cd.bin:
	git clone --depth=1 --branch=$(LIMINE_BRANCH) https://github.com/limine-bootloader/limine.git $(LIMINE_DIR)

clean:
	rm -rf build kernel/target
