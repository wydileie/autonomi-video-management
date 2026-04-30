.PHONY: help install-react test test-rust test-rust-admin clippy-rust clippy-rust-admin test-react build-react ci

NPM ?= npm
CARGO ?= cargo

help:
	@echo "Available targets:"
	@echo "  make test-rust       Run Rust stream cargo tests"
	@echo "  make test-rust-admin Run Rust admin cargo tests"
	@echo "  make clippy-rust     Run Rust stream clippy checks"
	@echo "  make clippy-rust-admin Run Rust admin clippy checks"
	@echo "  make install-react   Install React frontend dependencies"
	@echo "  make build-react     Build the React frontend"
	@echo "  make test-react      Run React tests in CI mode"
	@echo "  make test            Run all test suites"
	@echo "  make ci              Install dependencies and run CI checks"

test-rust:
	cd rust_stream && $(CARGO) test

test-rust-admin:
	cd rust_admin && $(CARGO) test

clippy-rust:
	cd rust_stream && $(CARGO) clippy --all-targets -- -D warnings

clippy-rust-admin:
	cd rust_admin && $(CARGO) clippy --all-targets -- -D warnings

install-react:
	cd react_frontend && $(NPM) ci

build-react:
	cd react_frontend && $(NPM) run build

test-react:
	cd react_frontend && CI=true $(NPM) test

test: test-rust test-rust-admin test-react

ci: test-rust test-rust-admin clippy-rust clippy-rust-admin install-react build-react test-react
