#!/usr/bin/env python3
"""
Populates Codex MCP server config from devcontainer.json by writing managed
MCP entries into ~/.codex/config.toml.
"""
import json
import os
import re
import shlex


ENV_PATTERN = re.compile(r"\$\{env:([A-Za-z_][A-Za-z0-9_]*)\}")
MANAGED_BEGIN = "# BEGIN AUTONOMI VIDEO MANAGEMENT MCP\n"
MANAGED_END = "# END AUTONOMI VIDEO MANAGEMENT MCP\n"


def resolve_workspace(value: str, workspace: str) -> str:
    return value.replace("${containerWorkspaceFolder}", workspace)


def contains_env_placeholder(value: str) -> bool:
    return bool(ENV_PATTERN.search(value))


def shell_expand_env(value: str) -> str:
    return ENV_PATTERN.sub(lambda match: "${%s}" % match.group(1), value)


def build_shell_command(command: str, args: list[str]) -> str:
    parts = [shlex.quote(command)]
    parts.extend(shlex.quote(shell_expand_env(arg)) for arg in args)
    return "exec " + " ".join(parts)


def toml_string(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def toml_array(values: list[str]) -> str:
    return "[" + ", ".join(toml_string(value) for value in values) + "]"


def strip_table(text: str, table_name: str) -> str:
    pattern = re.compile(
        rf"(?ms)^\[{re.escape(table_name)}\]\n.*?(?=^\[|^# BEGIN AUTONOMI VIDEO MANAGEMENT MCP|\Z)"
    )
    return re.sub(pattern, "", text)


def strip_managed_block(text: str) -> str:
    pattern = re.compile(
        rf"(?ms)^{re.escape(MANAGED_BEGIN)}.*?^{re.escape(MANAGED_END)}\n?"
    )
    return re.sub(pattern, "", text)


def render_server_block(name: str, cfg: dict, workspace: str) -> str:
    lines = [f"[mcp_servers.{name}]"]

    if "enabled" in cfg:
        lines.append(f"enabled = {str(bool(cfg['enabled'])).lower()}")
        if not cfg["enabled"] and "command" not in cfg and "url" not in cfg:
            lines.append("")
            return "\n".join(lines)

    if "url" in cfg:
        lines.append(f"url = {toml_string(cfg['url'])}")
        if "bearer_token_env_var" in cfg:
            lines.append(
                f"bearer_token_env_var = {toml_string(cfg['bearer_token_env_var'])}"
            )
        lines.append("")
        return "\n".join(lines)

    command = resolve_workspace(cfg["command"], workspace)
    args = [resolve_workspace(arg, workspace) for arg in cfg.get("args", [])]
    env = cfg.get("env", {})

    if any(contains_env_placeholder(arg) for arg in args):
        command = "/bin/bash"
        args = ["-lc", build_shell_command(command=cfg["command"], args=args)]

    lines.append(f"command = {toml_string(resolve_workspace(command, workspace))}")
    if args:
        lines.append(f"args = {toml_array(args)}")
    lines.append("")

    literal_env = {
        key: value
        for key, value in env.items()
        if not contains_env_placeholder(value)
    }
    if literal_env:
        lines.append(f"[mcp_servers.{name}.env]")
        for key, value in literal_env.items():
            lines.append(f"{key} = {toml_string(value)}")
        lines.append("")

    return "\n".join(lines)


def main() -> None:
    script_dir = os.path.dirname(os.path.abspath(__file__))
    workspace = os.path.dirname(script_dir)

    devcontainer_path = os.path.join(script_dir, "devcontainer.json")
    with open(devcontainer_path) as f:
        devcontainer = json.load(f)

    customizations = devcontainer.get("customizations", {})
    mcp_servers = dict(customizations.get("claude", {}).get("mcpServers", {}))
    for name, server_cfg in customizations.get("codex", {}).get("mcpServers", {}).items():
        if server_cfg.get("enabled") is False:
            mcp_servers.pop(name, None)
        else:
            mcp_servers[name] = server_cfg

    codex_dir = os.path.expanduser("~/.codex")
    os.makedirs(codex_dir, exist_ok=True)
    config_path = os.path.join(codex_dir, "config.toml")

    try:
        with open(config_path) as f:
            existing = f.read()
    except FileNotFoundError:
        existing = ""

    updated = strip_managed_block(existing)
    for name in mcp_servers:
        updated = strip_table(updated, f"mcp_servers.{name}.env")
        updated = strip_table(updated, f"mcp_servers.{name}")

    updated = updated.rstrip()
    if updated:
        updated += "\n\n"

    managed = [MANAGED_BEGIN]
    for name, cfg in mcp_servers.items():
        managed.append(render_server_block(name, cfg, workspace))
    managed.append(MANAGED_END)

    with open(config_path, "w") as f:
        f.write(updated + "".join(managed))

    print(f"Codex MCP servers written for workspace: {workspace}")


if __name__ == "__main__":
    main()
