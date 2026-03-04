SHELL := /bin/bash

CARGO ?= cargo
INSTALL_DIR ?= /usr/local/bin

.PHONY: help build release run check install install-rust install-shell clean

help:
	@echo "Targets:"
	@echo "  make build         Build the Rust binary (debug)"
	@echo "  make release       Build the Rust binary (release)"
	@echo "  make run           Run the Rust binary via cargo"
	@echo "  make check         Run cargo check"
	@echo "  make install       Install the Rust binary with cargo"
	@echo "  make install-rust  Install the Rust binary with cargo"
	@echo "  make install-shell Symlink ralph.sh to $(INSTALL_DIR)/ralph"
	@echo "  make clean         Remove build artifacts"

build:
	$(CARGO) build

release:
	$(CARGO) build --release

run:
	$(CARGO) run --bin ralph-rs --

check:
	$(CARGO) check

install: install-rust

install-rust:
	$(CARGO) install --path . --bin ralph-rs --force

install-shell:
	ln -sf "$(CURDIR)/ralph.sh" "$(INSTALL_DIR)/ralph"

clean:
	$(CARGO) clean
