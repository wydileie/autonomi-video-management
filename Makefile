.PHONY: help install-react test test-rust test-rust-admin test-antd clippy-rust clippy-rust-admin clippy-antd fmt-rust compose-config test-react build-react ci

NPM ?= npm
CARGO ?= cargo
DOCKER_COMPOSE ?= docker compose

help:
	@echo "Available targets:"
	@echo "  make test-rust       Run Rust stream cargo tests"
	@echo "  make test-rust-admin Run Rust admin cargo tests"
	@echo "  make test-antd       Run antd service cargo tests"
	@echo "  make clippy-rust     Run Rust stream clippy checks"
	@echo "  make clippy-rust-admin Run Rust admin clippy checks"
	@echo "  make clippy-antd     Run antd service clippy checks"
	@echo "  make fmt-rust        Check Rust formatting"
	@echo "  make compose-config  Validate Compose render paths"
	@echo "  make install-react   Install React frontend dependencies"
	@echo "  make build-react     Build the React frontend"
	@echo "  make test-react      Run React tests in CI mode"
	@echo "  make test            Run all test suites"
	@echo "  make ci              Install dependencies and run CI checks"

test-rust:
	cd rust_stream && $(CARGO) test

test-rust-admin:
	cd rust_admin && $(CARGO) test

test-antd:
	cd antd_service && $(CARGO) test

clippy-rust:
	cd rust_stream && $(CARGO) clippy --all-targets -- -D warnings

clippy-rust-admin:
	cd rust_admin && $(CARGO) clippy --all-targets -- -D warnings

clippy-antd:
	cd antd_service && $(CARGO) clippy --all-targets -- -D warnings

fmt-rust:
	cd rust_stream && $(CARGO) fmt --check
	cd rust_admin && $(CARGO) fmt --check
	cd antd_service && $(CARGO) fmt --check

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

test: test-rust test-rust-admin test-antd test-react

ci: fmt-rust test-rust test-rust-admin test-antd clippy-rust clippy-rust-admin clippy-antd install-react build-react test-react compose-config
