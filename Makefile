.PHONY: help install-react test test-rust test-rust-workspace test-rust-stream test-rust-admin test-antd clippy-rust clippy-rust-workspace clippy-rust-stream clippy-rust-admin clippy-antd fmt-rust compose-config test-react build-react audit-rust audit-react audit ci

NPM ?= npm
CARGO ?= cargo
DOCKER_COMPOSE ?= docker compose

help:
	@echo "Available targets:"
	@echo "  make test-rust       Run all Rust cargo tests"
	@echo "  make test-rust-workspace Run all Rust cargo tests"
	@echo "  make test-rust-stream Run Rust stream cargo tests"
	@echo "  make test-rust-admin Run Rust admin cargo tests"
	@echo "  make test-antd       Run antd service cargo tests"
	@echo "  make clippy-rust     Run all Rust clippy checks"
	@echo "  make clippy-rust-workspace Run all Rust clippy checks"
	@echo "  make clippy-rust-stream Run Rust stream clippy checks"
	@echo "  make clippy-rust-admin Run Rust admin clippy checks"
	@echo "  make clippy-antd     Run antd service clippy checks"
	@echo "  make fmt-rust        Check Rust formatting"
	@echo "  make compose-config  Validate Compose render paths"
	@echo "  make install-react   Install React frontend dependencies"
	@echo "  make build-react     Build the React frontend"
	@echo "  make test-react      Run React tests in CI mode"
	@echo "  make audit-rust      Run cargo audit if installed"
	@echo "  make audit-react     Run npm production audit"
	@echo "  make audit           Run advisory checks locally"
	@echo "  make test            Run all test suites"
	@echo "  make ci              Install dependencies and run CI checks"

test-rust:
	$(CARGO) test --workspace

test-rust-workspace: test-rust

test-rust-stream:
	$(CARGO) test -p rust_stream

test-rust-admin:
	$(CARGO) test -p rust_admin

test-antd:
	$(CARGO) test -p antd

clippy-rust:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

clippy-rust-workspace: clippy-rust

clippy-rust-stream:
	$(CARGO) clippy -p rust_stream --all-targets -- -D warnings

clippy-rust-admin:
	$(CARGO) clippy -p rust_admin --all-targets -- -D warnings

clippy-antd:
	$(CARGO) clippy -p antd --all-targets -- -D warnings

fmt-rust:
	$(CARGO) fmt --all --check

compose-config:
	$(DOCKER_COMPOSE) --env-file .env.local.example -f docker-compose.yml -f docker-compose.local.yml config >/tmp/autvid-compose-local.yml
	$(DOCKER_COMPOSE) --env-file .env.local-public.example -f docker-compose.yml -f docker-compose.local.yml -f docker-compose.local-public.yml config >/tmp/autvid-compose-local-public.yml
	$(DOCKER_COMPOSE) --env-file .env.production.example -f docker-compose.yml -f docker-compose.prod.yml config >/tmp/autvid-compose-prod.yml

install-react:
	cd react_frontend && $(NPM) ci

build-react:
	cd react_frontend && $(NPM) run build

test-react:
	cd react_frontend && CI=true $(NPM) test

audit-rust:
	$(CARGO) audit

audit-react:
	cd react_frontend && $(NPM) audit --omit=dev

audit: audit-rust audit-react

test: test-rust test-react

ci: fmt-rust test-rust clippy-rust install-react build-react test-react compose-config
