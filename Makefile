.PHONY: help install-react install-desktop test test-rust test-rust-workspace test-rust-stream test-rust-admin test-rust-db test-antd clippy-rust clippy-rust-workspace clippy-rust-stream clippy-rust-admin clippy-antd fmt-rust compose-config up-local up-local-full up-prod down-local down-prod logs logs-prod logs-monitoring devbench-build devbench-up devbench-down devbench-restart devbench-status devbench-shell devbench-exec backup-production restore-production lint-react test-react coverage-react build-react stage-tauri-sidecars build-tauri smoke-local smoke-local-restart smoke-local-large-original audit-rust audit-react audit-trivy audit ci

NPM ?= npm
CARGO ?= cargo
DOCKER_COMPOSE ?= docker compose
LOCAL_ENV ?= .env.local
PROD_ENV ?= .env.production
LOCAL_COMPOSE_FILES = -f docker-compose.yml -f docker-compose.local.yml
LOCAL_MONITORING_COMPOSE_FILES = $(LOCAL_COMPOSE_FILES) -f docker-compose.monitoring.yml
LOCAL_FULL_COMPOSE_FILES = $(LOCAL_MONITORING_COMPOSE_FILES) -f docker-compose.logging.yml
PROD_COMPOSE_FILES = -f docker-compose.yml -f docker-compose.prod.yml
CORE_LOG_SERVICES = rust_admin rust_stream antd nginx react_frontend
MONITORING_LOG_SERVICES = prometheus alertmanager grafana loki promtail

help:
	@echo "Available targets:"
	@echo "  make test-rust       Run all Rust cargo tests"
	@echo "  make test-rust-workspace Run all Rust cargo tests"
	@echo "  make test-rust-stream Run Rust stream cargo tests"
	@echo "  make test-rust-admin Run Rust admin cargo tests"
	@echo "  make test-rust-db    Run rust_admin SQLite-backed integration tests"
	@echo "  make test-antd       Run antd service cargo tests"
	@echo "  make clippy-rust     Run all Rust clippy checks"
	@echo "  make clippy-rust-workspace Run all Rust clippy checks"
	@echo "  make clippy-rust-stream Run Rust stream clippy checks"
	@echo "  make clippy-rust-admin Run Rust admin clippy checks"
	@echo "  make clippy-antd     Run antd service clippy checks"
	@echo "  make fmt-rust        Check Rust formatting"
	@echo "  make compose-config  Validate Compose render paths"
	@echo "  make up-local        Start local devnet stack in the background"
	@echo "  make up-local-full   Start local devnet stack with monitoring and logging"
	@echo "  make up-prod         Start production/default-network stack"
	@echo "  make down-local      Stop local devnet stack without deleting volumes"
	@echo "  make down-prod       Stop production stack without deleting volumes"
	@echo "  make logs            Follow local core service logs"
	@echo "  make logs-prod       Follow production core service logs"
	@echo "  make logs-monitoring Follow local monitoring service logs"
	@echo "  make devbench-up     Run the VS Code devcontainer image headlessly"
	@echo "  make devbench-shell  Open a shell in the headless devcontainer test bench"
	@echo "  make devbench-exec ARGS='make test-rust' Run a command in the headless devcontainer"
	@echo "  make devbench-down   Stop the headless devcontainer test bench"
	@echo "  make backup-production Create a timestamped production SQLite/catalog backup"
	@echo "  make restore-production ARGS='--backup-dir backups/autvid-... --yes' Restore a production backup"
	@echo "  make install-react   Install React frontend dependencies"
	@echo "  make install-desktop Install Tauri desktop build dependencies"
	@echo "  make lint-react      Run React ESLint checks"
	@echo "  make build-react     Build the React frontend"
	@echo "  make stage-tauri-sidecars Build and stage desktop sidecar binaries"
	@echo "  make build-tauri     Build the Tauri desktop app bundles"
	@echo "  make test-react      Run React tests in CI mode"
	@echo "  make smoke-local     Run an end-to-end local devnet smoke test"
	@echo "  make smoke-local-restart Run smoke test with rust_admin restart recovery"
	@echo "  make smoke-local-large-original Run smoke test with >16MB original source upload"
	@echo "  make audit-rust      Run cargo audit if installed"
	@echo "  make audit-react     Run npm production audit"
	@echo "  make audit-trivy     Run Trivy filesystem scan if installed"
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

test-rust-db:
	$(CARGO) test -p rust_admin --features db-tests db_tests -- --test-threads=1

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
	$(DOCKER_COMPOSE) --env-file .env.local.example $(LOCAL_FULL_COMPOSE_FILES) config >/tmp/autvid-compose-local-full.yml
	$(DOCKER_COMPOSE) --env-file .env.local.example $(LOCAL_COMPOSE_FILES) -f docker-compose.backup.yml config >/tmp/autvid-compose-backup.yml
	$(DOCKER_COMPOSE) --env-file .env.production.example -f docker-compose.yml -f docker-compose.prod.yml config >/tmp/autvid-compose-prod.yml
	$(DOCKER_COMPOSE) --env-file .env.production.example -f docker-compose.yml -f docker-compose.prod.yml -f docker-compose.monitoring.yml -f docker-compose.logging.yml config >/tmp/autvid-compose-prod-observability.yml
	$(DOCKER_COMPOSE) --env-file .env.production.example -f docker-compose.yml -f docker-compose.prod.yml -f docker-compose.backup.yml config >/tmp/autvid-compose-prod-backup.yml

up-local:
	$(DOCKER_COMPOSE) --env-file $(LOCAL_ENV) $(LOCAL_COMPOSE_FILES) up --build -d

up-local-full:
	$(DOCKER_COMPOSE) --env-file $(LOCAL_ENV) $(LOCAL_FULL_COMPOSE_FILES) up --build -d

up-prod:
	$(DOCKER_COMPOSE) --env-file $(PROD_ENV) $(PROD_COMPOSE_FILES) up --build -d

down-local:
	$(DOCKER_COMPOSE) --env-file $(LOCAL_ENV) $(LOCAL_FULL_COMPOSE_FILES) down --remove-orphans

down-prod:
	$(DOCKER_COMPOSE) --env-file $(PROD_ENV) $(PROD_COMPOSE_FILES) down

logs:
	$(DOCKER_COMPOSE) --env-file $(LOCAL_ENV) $(LOCAL_COMPOSE_FILES) logs -f $(CORE_LOG_SERVICES)

logs-prod:
	$(DOCKER_COMPOSE) --env-file $(PROD_ENV) $(PROD_COMPOSE_FILES) logs -f $(CORE_LOG_SERVICES)

logs-monitoring:
	$(DOCKER_COMPOSE) --env-file $(LOCAL_ENV) $(LOCAL_FULL_COMPOSE_FILES) logs -f $(MONITORING_LOG_SERVICES)

devbench-build:
	scripts/devcontainer-testbench.sh build

devbench-up:
	scripts/devcontainer-testbench.sh up

devbench-down:
	scripts/devcontainer-testbench.sh down

devbench-restart:
	scripts/devcontainer-testbench.sh restart

devbench-status:
	scripts/devcontainer-testbench.sh status

devbench-shell:
	scripts/devcontainer-testbench.sh shell

devbench-exec:
	@test -n "$(ARGS)" || { echo "Usage: make devbench-exec ARGS='make test-rust'"; exit 2; }
	scripts/devcontainer-testbench.sh exec $(ARGS)

backup-production:
	scripts/backup-production.sh

restore-production:
	@test -n "$(ARGS)" || { echo "Usage: make restore-production ARGS='--backup-dir backups/autvid-YYYYMMDDTHHMMSSZ --yes'"; exit 2; }
	scripts/restore-production.sh $(ARGS)

install-react:
	cd react_frontend && $(NPM) ci

install-desktop:
	cd desktop_app && $(NPM) ci

build-react:
	cd react_frontend && $(NPM) run build

stage-tauri-sidecars:
	scripts/stage-tauri-sidecars.sh

build-tauri: install-react install-desktop stage-tauri-sidecars
	cd desktop_app && $(NPM) run tauri -- build

lint-react:
	cd react_frontend && $(NPM) run lint

test-react:
	cd react_frontend && CI=true $(NPM) test

coverage-react:
	cd react_frontend && CI=true $(NPM) run test:coverage

smoke-local:
	scripts/smoke-local-devnet.sh

smoke-local-restart:
	scripts/smoke-local-devnet.sh --restart-admin

smoke-local-large-original:
	scripts/smoke-local-devnet.sh --large-original

audit-rust:
	@if command -v cargo-audit >/dev/null 2>&1; then \
		$(CARGO) audit; \
	else \
		echo "cargo-audit is not installed; skipping Rust advisory scan"; \
	fi

audit-react:
	cd react_frontend && $(NPM) audit --omit=dev

audit-trivy:
	@if command -v trivy >/dev/null 2>&1; then \
		trivy fs --ignore-unfixed --severity CRITICAL,HIGH .; \
	else \
		echo "trivy is not installed; skipping filesystem image/dependency scan"; \
	fi

audit: audit-rust audit-react audit-trivy

test: test-rust test-react

ci: fmt-rust test-rust clippy-rust install-react lint-react build-react test-react compose-config
