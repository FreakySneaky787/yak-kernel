# Chapter 7 - The keyboard.
#   make run    build the kernel and boot it in a QEMU window (serial output in this terminal);
#               KERNEL_CMDLINE=fault=page make run (or fault=stack) crashes it on purpose
#   make test   run the unit tests inside the kernel (`cargo test`), then boot the normal kernel, type on its
#               keyboard and check its output - and boot it twice more with a fault on purpose, which the handlers
#               must report
LIMINE_DIR ?= $(CURDIR)/vendor/limine
LIMINE_BRANCH := v11.x-binary
KERNEL := kernel/target/x86_64-unknown-none/debug/kernel
export LIMINE_DIR
# what the check types: "Hi YAK!" with Shift and Caps Lock, then Enter (QEMU key names = US key positions)
TYPED := shift-h i spc caps_lock y a k caps_lock shift-1 ret

.PHONY: run test clean

run: $(LIMINE_DIR)/limine-bios-cd.bin
	cd kernel && cargo run

test: $(LIMINE_DIR)/limine-bios-cd.bin
	cd kernel && cargo test
	cd kernel && cargo build
	KERNEL_MODE=check KEYS_AFTER="type something" KEYS="$(TYPED)" ./run-qemu.sh $(KERNEL)
	KERNEL_MODE=check KERNEL_CMDLINE=fault=page EXPECTED=expected-page-fault.txt ./run-qemu.sh $(KERNEL)
	KERNEL_MODE=check KERNEL_CMDLINE=fault=stack EXPECTED=expected-double-fault.txt ./run-qemu.sh $(KERNEL)

# The Limine bootloader is downloaded, not part of our code
$(LIMINE_DIR)/limine-bios-cd.bin:
	git clone --depth=1 --branch=$(LIMINE_BRANCH) https://github.com/limine-bootloader/limine.git $(LIMINE_DIR)

clean:
	rm -rf build kernel/target
