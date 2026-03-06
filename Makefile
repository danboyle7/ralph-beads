SHELL := /bin/bash

CARGO ?= cargo

.PHONY: help build release run check install version verify-installed-version clean

help:
	@echo "Targets:"
	@echo "  make build         Build the Rust binary (debug)"
	@echo "  make release       Build the Rust binary (release)"
	@echo "  make run           Run the Rust binary via cargo"
	@echo "  make check         Run cargo check"
	@echo "  make install       Install the Rust binary with cargo"
	@echo "  make version       Print workspace binary version/build info"
	@echo "  make verify-installed-version Compare workspace vs installed `ralph --version`"
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

version:
	$(CARGO) run --quiet --bin ralph -- --version

verify-installed-version:
	@expected="$$( $(CARGO) run --quiet --bin ralph -- --version )"; \
	if ! command -v ralph >/dev/null 2>&1; then \
		echo "ralph not found in PATH. Run: make install"; \
		exit 1; \
	fi; \
	actual="$$( ralph --version 2>&1 || true )"; \
	if [[ "$$actual" != "$$expected" ]]; then \
		echo "Installed ralph does not match this workspace build."; \
		echo "--- workspace ---"; \
		echo "$$expected"; \
		echo "--- installed ---"; \
		echo "$$actual"; \
		echo "Run: make install"; \
		exit 1; \
	fi; \
	echo "Installed ralph matches this workspace build."; \
	echo "$$actual"

clean:
	$(CARGO) clean
