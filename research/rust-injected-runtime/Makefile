SHELL := /bin/bash

HOST := $(shell rustc -vV | sed -n 's/^host: //p')
SYSROOT := $(shell rustc --print sysroot)
LLVM_BIN := $(SYSROOT)/lib/rustlib/$(HOST)/bin
RUST_LLD := $(LLVM_BIN)/rust-lld
COMMON_RUSTFLAGS := -C relocation-model=pic -C no-redzone=yes -C force-unwind-tables=no -C overflow-checks=no

.PHONY: all check smoke-x86_64 clean

all: target/blob-x86_64.bin target/blob-aarch64.bin

check: all
	@bash ./verify.sh target/blob-x86_64.elf target/blob-x86_64.bin $(LLVM_BIN)
	@bash ./verify.sh target/blob-aarch64.elf target/blob-aarch64.bin $(LLVM_BIN)
	@if [[ "$$(uname -s)-$$(uname -m)" == Linux-x86_64 ]]; then \
		$(MAKE) --no-print-directory smoke-x86_64; \
	fi

smoke-x86_64: target/smoke-x86_64 target/blob-x86_64.bin
	@symbols="$$($(LLVM_BIN)/llvm-nm --numeric-sort target/blob-x86_64.elf)"; \
		state_ptr=$$(awk '$$3 == "FSPY_STATE_PTR" { print "0x" $$1 }' <<< "$$symbols"); \
		handler=$$(awk '$$3 == "fspy_sigsys_handler" { print "0x" $$1 }' <<< "$$symbols"); \
		restorer=$$(awk '$$3 == "fspy_rt_sigreturn" { print "0x" $$1 }' <<< "$$symbols"); \
		allocator=$$(awk '$$3 == "fspy_alloc" { print "0x" $$1 }' <<< "$$symbols"); \
		raw_syscall=$$(awk '$$3 == "fspy_raw_syscall6" { print "0x" $$1 }' <<< "$$symbols"); \
		target/smoke-x86_64 target/blob-x86_64.bin \
			"$$state_ptr" "$$handler" "$$restorer" "$$allocator" "$$raw_syscall"

target/smoke-x86_64: smoke_x86_64.c
	@mkdir -p target
	$(CC) -O2 -Wall -Wextra -Werror -std=gnu11 -o $@ $<

target/x86_64/x86_64-unknown-linux-musl/release/libfspy_rust_injected_runtime.a: Cargo.toml src/lib.rs
	RUSTFLAGS='$(COMMON_RUSTFLAGS)' cargo build --release \
		--target x86_64-unknown-linux-musl --target-dir target/x86_64

target/aarch64/aarch64-unknown-linux-musl/release/libfspy_rust_injected_runtime.a: Cargo.toml src/lib.rs
	RUSTFLAGS='$(COMMON_RUSTFLAGS) -C target-feature=-outline-atomics' cargo build --release \
		--target aarch64-unknown-linux-musl --target-dir target/aarch64

target/blob-x86_64.elf: target/x86_64/x86_64-unknown-linux-musl/release/libfspy_rust_injected_runtime.a blob.ld
	$(RUST_LLD) -flavor gnu -m elf_x86_64 -nostdlib --gc-sections \
		--no-undefined --build-id=none -T blob.ld -o $@ $<

target/blob-aarch64.elf: target/aarch64/aarch64-unknown-linux-musl/release/libfspy_rust_injected_runtime.a blob.ld
	$(RUST_LLD) -flavor gnu -m aarch64linux -nostdlib --gc-sections \
		--no-undefined --build-id=none -T blob.ld -o $@ $<

target/blob-x86_64.bin: target/blob-x86_64.elf
	$(LLVM_BIN)/llvm-objcopy --only-section=.fspy_blob -O binary $< $@

target/blob-aarch64.bin: target/blob-aarch64.elf
	$(LLVM_BIN)/llvm-objcopy --only-section=.fspy_blob -O binary $< $@

clean:
	rm -rf target
