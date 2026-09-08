#!/usr/bin/env bash
set -euo pipefail
if [[ "${AUTVID_CONFIGURE_AGENT_MCP:-false}" == true ]]; then
  python3 .devcontainer/setup_claude.py
fi
