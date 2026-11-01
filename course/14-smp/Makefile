# Chapter 14 - The other CPUs.
#   KERNEL_SMP=1 make test (or 2, ...) runs it with another number of CPUs; 4 if not set
#   make run    build the kernel and boot it in a QEMU window (serial output in this terminal);
#               KERNEL_CMDLINE=fault=page make run (or fault=stack, fault=overflow, fault=heap) crashes it on purpose
#   make test   run the unit tests inside the kernel (`cargo test`), then boot the normal kernel, type five
#               commands into its shell and check their output - and boot it twice more with a fault on purpose,
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
TYPED := e c h o spc shift-h i spc spc caps_lock y a k caps_lock shift-1 ret \
	e c h o spc s h e l x backspace l ret \
	n o p e ret \
	m e m ret \
	t h r e a d s ret

.PHONY: run test clean

run: $(LIMINE_DIR)/limine-bios-cd.bin
	cd kernel && cargo run

test: $(LIMINE_DIR)/limine-bios-cd.bin
	cd kernel && cargo test
	cd kernel && cargo build
	KERNEL_MODE=check KEYS_AFTER="Shell: type help" KEYS="$(TYPED)" ./run-qemu.sh $(KERNEL)
	KERNEL_MODE=check KERNEL_CMDLINE=fault=page EXPECTED=expected-page-fault.txt ./run-qemu.sh $(KERNEL)
	KERNEL_MODE=check KERNEL_CMDLINE=fault=stack EXPECTED=expected-double-fault.txt ./run-qemu.sh $(KERNEL)
	KERNEL_MODE=check KERNEL_CMDLINE=fault=overflow EXPECTED=expected-overflow.txt ./run-qemu.sh $(KERNEL)
	KERNEL_MODE=check KERNEL_CMDLINE=fault=heap EXPECTED=expected-heap.txt ./run-qemu.sh $(KERNEL)

# The Limine bootloader is downloaded, not part of our code
$(LIMINE_DIR)/limine-bios-cd.bin:
	git clone --depth=1 --branch=$(LIMINE_BRANCH) https://github.com/limine-bootloader/limine.git $(LIMINE_DIR)

clean:
	rm -rf build kernel/target
