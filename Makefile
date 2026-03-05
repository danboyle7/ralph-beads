SHELL := /bin/bash

CARGO ?= cargo

.PHONY: help build release run check install clean

help:
	@echo "Targets:"
	@echo "  make build         Build the Rust binary (debug)"
	@echo "  make release       Build the Rust binary (release)"
	@echo "  make run           Run the Rust binary via cargo"
	@echo "  make check         Run cargo check"
	@echo "  make install       Install the Rust binary with cargo"
	@echo "  make clean         Remove build artifacts"

build:
	$(CARGO) build

release:
	$(CARGO) build --release

run:
	$(CARGO) run --bin ralph --

check:
	$(CARGO) check

install:
	$(CARGO) install --path . --bin ralph --force

clean:
	$(CARGO) clean
