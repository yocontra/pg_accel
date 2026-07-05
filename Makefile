# Thin wrapper around Justfile for make users.
# All logic lives in Justfile — this just delegates.

.PHONY: pre-commit test test-unit bench fmt lint check clear-jit gpu-build gpu-test gpu-test-cold-all setup-gpu-metal

pre-commit:
	just pre-commit

test:
	just test

test-unit:
	just test-unit

bench:
	just bench

fmt:
	just fmt

lint:
	just lint

check:
	just check

# Clear AdaptiveCpp Metal SSCP JIT cache (~/.acpp/apps/global/jit-cache).
# Use this before any test that needs a cold-cache run (fork-safety,
# archive-builder behaviour, kernel re-compilation after a code change).
clear-jit:
	just clear-jit

gpu-build:
	just gpu-build

gpu-test:
	just gpu-test

gpu-test-cold-all:
	just gpu-test-cold-all

setup-gpu-metal:
	just setup-gpu-metal
