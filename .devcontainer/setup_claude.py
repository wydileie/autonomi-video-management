#!/usr/bin/env python3
"""
Populates the mcpServers section of ~/.claude.json from devcontainer.json.

The Claude MCP block is used as the base, then Codex MCP overrides are applied
so both CLIs see the same useful server set. Codex-style remote HTTP entries
are translated into Claude Code's config format.
"""
import json
import os
from copy import deepcopy

# Workspace folder is the parent of the .devcontainer directory
script_dir = os.path.dirname(os.path.abspath(__file__))
workspace = os.path.dirname(script_dir)

devcontainer_path = os.path.join(script_dir, "devcontainer.json")
with open(devcontainer_path) as f:
    devcontainer = json.load(f)

customizations = devcontainer.get("customizations", {})
claude_servers = customizations.get("claude", {}).get("mcpServers", {})
codex_servers = customizations.get("codex", {}).get("mcpServers", {})

mcp_servers_raw = deepcopy(claude_servers)
for name, cfg in codex_servers.items():
    if cfg.get("enabled") is False:
        mcp_servers_raw.pop(name, None)
    else:
        mcp_servers_raw[name] = cfg


def resolve_value(value):
    if isinstance(value, list):
        return [resolve_value(item) for item in value]
    if isinstance(value, dict):
        return {key: resolve_value(item) for key, item in value.items()}
    if isinstance(value, str):
        return value.replace("${containerWorkspaceFolder}", workspace)
    return value


def claude_entry(cfg):
    cfg = resolve_value(cfg)
    if "url" in cfg:
        url = cfg["url"]
        token_env = cfg.get("bearer_token_env_var")
        if token_env:
            helper_path = os.path.join(
                os.path.expanduser("~"), ".local", "bin", f"{token_env.lower()}-mcp-remote"
            )
            os.makedirs(os.path.dirname(helper_path), exist_ok=True)
            with open(helper_path, "w") as helper:
                helper.write(
                    "#!/bin/sh\n"
                    "set -eu\n"
                    f'token="${{{token_env}:-}}"\n'
                    'if [ -z "$token" ]; then\n'
                    f'  echo "{token_env} is not available in the environment." >&2\n'
                    "  exit 1\n"
                    "fi\n"
                    "exec npx -y mcp-remote@latest "
                    f"'{url}' "
                    '--header "Authorization:Bearer $token" '
                    "--transport http-only\n"
                )
            os.chmod(helper_path, 0o700)
            return {
                "type": "stdio",
                "command": helper_path,
                "args": [],
            }
        return {"type": "http", "url": url}

    entry = {"type": "stdio"}
    for key, value in cfg.items():
        if key == "enabled":
            continue
        entry[key] = value
    if "env" not in entry:
        entry["env"] = {}
    return entry

# Resolve ${containerWorkspaceFolder} and build entries with correct Claude format
mcp_servers = {}
for name, cfg in mcp_servers_raw.items():
    mcp_servers[name] = claude_entry(cfg)

# Merge into ~/.claude.json, preserving all other keys
claude_json_path = os.path.expanduser("~/.claude.json")
try:
    with open(claude_json_path) as f:
        claude_config = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    claude_config = {}

claude_config["mcpServers"] = mcp_servers

with open(claude_json_path, "w") as f:
    json.dump(claude_config, f, indent=2)

print(f"Claude MCP servers written to ~/.claude.json for workspace: {workspace}")
