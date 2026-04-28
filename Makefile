.PHONY: help install-python install-react test test-python test-rust test-react build-react ci

PYTHON ?= python3
PIP ?= $(PYTHON) -m pip
NPM ?= npm
CARGO ?= cargo

help:
	@echo "Available targets:"
	@echo "  make install-python  Install Python admin dependencies"
	@echo "  make test-python     Run Python admin unittest discovery"
	@echo "  make test-rust       Run Rust stream cargo tests"
	@echo "  make install-react   Install React frontend dependencies"
	@echo "  make build-react     Build the React frontend"
	@echo "  make test-react      Run React tests in CI mode"
	@echo "  make test            Run all test suites"
	@echo "  make ci              Install dependencies and run CI checks"

install-python:
	$(PIP) install -r python_admin/requirements.txt

test-python:
	cd python_admin && $(PYTHON) -m unittest discover -s tests -p "test*.py" -v

test-rust:
	cd rust_stream && $(CARGO) test

install-react:
	cd react_frontend && $(NPM) ci

build-react:
	cd react_frontend && $(NPM) run build

test-react:
	cd react_frontend && CI=true $(NPM) test -- --watchAll=false

test: test-python test-rust test-react

ci: install-python test-python test-rust install-react build-react test-react
