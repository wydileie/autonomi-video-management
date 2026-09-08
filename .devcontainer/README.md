# Development container and headless test bench

VS Code and `make devbench-*` use the same Dockerfile. The image pins Rust 1.98.1,
Node 24.20.0, ant-core's compatible ant-node/devnet 0.18.1, upstream SDK 0.12.1,
ant CLI 0.3.6, and Foundry 1.8.1. Foundry archives are checked against release SHA-256 hashes.

The application gateway is built from `crates/antd_service` and uses port 8082.
Upstream SDK tooling is named `antd-upstream`, uses loopback port 8182, and serves
`antd-mcp`. Application smoke tests must exercise the application gateway.

Start the bench with `make devbench-up`, run `make devbench-exec ARGS='make test-rust'`,
then remove its container with `make devbench-down`. The reusable image may remain.
Published bench ports bind to 127.0.0.1. Override DEVBENCH_HTTP_PORT,
DEVBENCH_ADMIN_PORT, DEVBENCH_STREAM_PORT, and DEVBENCH_ANTD_REST_PORT if another
local stack uses them. The bench mounts the host Docker socket and optional agent
configurations, so use it only with trusted repository code.

Optional services and configuration are opt-in:

- `AUTVID_START_UPSTREAM_TOOLING=true make devbench-up` starts a local devnet and
  upstream SDK daemon. In VS Code, set that variable in `containerEnv` or run
  `bash .devcontainer/start_autonomi.sh` explicitly. This local devnet is disposable.
- `AUTVID_CONFIGURE_AGENT_MCP=true` enables the managed Claude/Codex MCP setup
  scripts. Existing agent configuration remains untouched by default.
- Docker build argument `INSTALL_EXTRA_TOOLS=true` requires explicit
  `KUBECTL_VERSION`, `HELM_VERSION`, and `POWERSHELL_VERSION` pins. These tools
  are unnecessary for the application and are omitted by default.

`ANTD_NETWORK=local` selects the funded local test wallet from the devnet manifest.
For public networks, configure credentials explicitly. Never copy real wallet
keys into this directory or commit generated agent configuration.
