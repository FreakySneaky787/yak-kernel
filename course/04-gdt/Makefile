# Chapter 4 - GDT and TSS.
#   make run    build the kernel and boot it in a QEMU window (serial output in this terminal)
#   make test   run the unit tests inside the kernel (`cargo test`), then boot the normal kernel and check its output
LIMINE_DIR ?= $(CURDIR)/vendor/limine
LIMINE_BRANCH := v11.x-binary
export LIMINE_DIR

.PHONY: run test clean

run: $(LIMINE_DIR)/limine-bios-cd.bin
	cd kernel && cargo run

test: $(LIMINE_DIR)/limine-bios-cd.bin
	cd kernel && cargo test
	cd kernel && cargo build
	KERNEL_MODE=check ./run-qemu.sh kernel/target/x86_64-unknown-none/debug/kernel

# The Limine bootloader is downloaded, not part of our code
$(LIMINE_DIR)/limine-bios-cd.bin:
	git clone --depth=1 --branch=$(LIMINE_BRANCH) https://github.com/limine-bootloader/limine.git $(LIMINE_DIR)

clean:
	rm -rf build kernel/target
