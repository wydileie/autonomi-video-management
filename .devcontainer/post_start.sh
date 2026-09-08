#!/usr/bin/env bash
set -euo pipefail

if [[ "${AUTVID_START_UPSTREAM_TOOLING:-false}" == true ]]; then
  bash .devcontainer/start_autonomi.sh
fi
if [[ "${AUTVID_CONFIGURE_AGENT_MCP:-false}" == true ]]; then
  python3 .devcontainer/setup_codex.py
fi
