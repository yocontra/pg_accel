# Thin wrapper around Justfile for make users.
# All logic lives in Justfile — this just delegates.

.PHONY: pre-commit test test-unit test-integration bench fmt lint check

pre-commit:
	just pre-commit

test:
	just test

test-unit:
	just test-unit

test-integration:
	just test-integration

bench:
	just bench

fmt:
	just fmt

lint:
	just lint

check:
	just check
