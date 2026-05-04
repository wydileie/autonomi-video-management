FROM mcr.microsoft.com/devcontainers/python:1-3.12-bookworm

ENV DEBIAN_FRONTEND=noninteractive
ENV VIRTUAL_ENV=/opt/venv
ENV CARGO_HOME=/usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup
ENV PATH="/opt/venv/bin:${PATH}:/opt/mssql-tools18/bin:${CARGO_HOME}/bin"

# Keep the Autonomi toolchain on a single ant-node/protocol line.
ARG AUTONOMI_ANT_SDK_REF=v0.5.1
ARG AUTONOMI_ANT_CLIENT_REF=ant-cli-v0.2.1
ARG AUTONOMI_ANT_NODE_REF=v0.11.0

# Install all system packages and tools in fewer layers
RUN rm -f /etc/apt/sources.list.d/yarn.list \
&& apt-get update && apt-get install -y \
curl \
wget \
git \
git-lfs \
gnupg \
ca-certificates \
jq \
software-properties-common \
unzip \
postgresql-client \
default-mysql-client \
python3-venv \
build-essential \
python3-dev \
libssl-dev \
protobuf-compiler \
docker.io \
ripgrep \
dnsutils \
net-tools \
netcat-openbsd \
nmap \
fd-find \
bat \
fzf \
htop \
tree \
tcpdump \
ffmpeg \
&& rm -rf /var/lib/apt/lists/*

# Install Node.js 24 and the Python package runner used by MCP helpers.
RUN mkdir -p /etc/apt/keyrings \
&& curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg \
&& echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_24.x nodistro main" > /etc/apt/sources.list.d/nodesource.list \
&& apt-get update \
&& apt-get install -y nodejs \
&& pip install --no-cache-dir uv \
&& ln -s /usr/bin/fdfind /usr/local/bin/fd \
&& ln -s /usr/bin/batcat /usr/local/bin/bat \
&& rm -rf /var/lib/apt/lists/*

# Install Rust and Rust development tools
RUN mkdir -p "${CARGO_HOME}" "${RUSTUP_HOME}" \
&& chmod -R a+rwX "${CARGO_HOME}" "${RUSTUP_HOME}" \
&& curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path \
&& rustup component add rustfmt clippy \
&& cargo install cargo-edit cargo-outdated \
&& PW_ARCH="$(dpkg --print-architecture)" \
&& case "$PW_ARCH" in \
amd64) PS_ARCH="x64" ;; \
arm64) PS_ARCH="arm64" ;; \
*) echo "Unsupported PowerShell architecture: $PW_ARCH" >&2; exit 1 ;; \
esac \
&& PS_VERSION="$(curl -fsSL https://api.github.com/repos/PowerShell/PowerShell/releases/latest | grep -m1 '\"tag_name\":' | sed -E 's/.*\"v?([^\"]+)\".*/\1/')" \
&& { [ -n "$PS_VERSION" ] || { echo "ERROR: Failed to determine PowerShell version from GitHub API" >&2; exit 1; }; } \
&& curl -fsSLo /tmp/powershell.tar.gz "https://github.com/PowerShell/PowerShell/releases/download/v${PS_VERSION}/powershell-${PS_VERSION}-linux-${PS_ARCH}.tar.gz" \
&& mkdir -p /opt/microsoft/powershell/7 \
&& tar -xzf /tmp/powershell.tar.gz -C /opt/microsoft/powershell/7 \
&& chmod +x /opt/microsoft/powershell/7/pwsh \
&& ln -sf /opt/microsoft/powershell/7/pwsh /usr/bin/pwsh \
&& rm -f /tmp/powershell.tar.gz \
&& pwsh --version \
&& chmod -R a+rwX "${CARGO_HOME}" "${RUSTUP_HOME}" \
&& rm -rf /var/lib/apt/lists/*

# Build and install antd (Autonomi daemon), the Autonomi MCP server, and the
# dev CLI support libraries from ant-sdk in a single layer to share the cargo cache.
RUN git clone https://github.com/WithAutonomi/ant-sdk.git && \
    cd ant-sdk && \
    git checkout "$AUTONOMI_ANT_SDK_REF" && \
    cd antd && \
    cargo build --release && \
    cp target/release/antd /usr/local/bin/ && \
    cd ../antd-py && \
    pip install --no-cache-dir ".[rest]" && \
    cd ../antd-mcp && \
    pip install --no-cache-dir . && \
    cd ../.. && rm -rf ant-sdk

# Build and install ant (Autonomi CLI)
RUN git clone https://github.com/WithAutonomi/ant-client.git && \
    cd ant-client && \
    git checkout "$AUTONOMI_ANT_CLIENT_REF" && \
    cargo build --release --bin ant && \
    cp target/release/ant /usr/local/bin/ && \
    cd .. && rm -rf ant-client

# Build ant-devnet from ant-node — required for the local devnet used in development.
# ant dev start normally does this via `cargo run`; we pre-build it here so startup
# is instant and no source repos are needed at runtime.
RUN git clone https://github.com/WithAutonomi/ant-node.git && \
    cd ant-node && \
    git checkout "$AUTONOMI_ANT_NODE_REF" && \
    cargo build --release --bin ant-devnet && \
    cp target/release/ant-devnet /usr/local/bin/ && \
    cd .. && rm -rf ant-node

# Install Foundry (anvil) — required by ant-devnet to run the local EVM chain.
# Downloads pre-built binaries directly from GitHub releases (more reliable in Docker
# than the foundryup installer script which can fail on rate limits or interactive prompts).
RUN ARCH=$(dpkg --print-architecture) && \
    case "$ARCH" in \
      amd64) FOUNDRY_ARCH="amd64" ;; \
      arm64) FOUNDRY_ARCH="arm64" ;; \
      *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;; \
    esac && \
    FOUNDRY_TAG=$(curl -fsSL https://api.github.com/repos/foundry-rs/foundry/releases/latest | grep '"tag_name"' | head -1 | sed -E 's/.*"([^"]+)".*/\1/') && \
    { [ -n "$FOUNDRY_TAG" ] || { echo "ERROR: could not determine Foundry release tag" >&2; exit 1; }; } && \
    curl -fsSLo /tmp/foundry.tar.gz \
      "https://github.com/foundry-rs/foundry/releases/download/${FOUNDRY_TAG}/foundry_${FOUNDRY_TAG}_linux_${FOUNDRY_ARCH}.tar.gz" && \
    tar -xzf /tmp/foundry.tar.gz -C /usr/local/bin anvil forge cast chisel && \
    rm /tmp/foundry.tar.gz && \
    anvil --version

# Create docker group and add vscode so the Docker socket (mounted from OrbStack
# at runtime as root:root 0660) becomes accessible after postStartCommand chgrps it.
RUN groupadd -f docker && usermod -aG docker vscode

# Install kubectl and Helm for the current architecture.
RUN ARCH="$(dpkg --print-architecture)" \
&& case "$ARCH" in \
amd64) K8S_ARCH="amd64" ;; \
arm64) K8S_ARCH="arm64" ;; \
*) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;; \
esac \
&& KUBECTL_VERSION="$(curl -fsSL https://dl.k8s.io/release/stable.txt)" \
&& curl -fsSLo /usr/local/bin/kubectl "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${K8S_ARCH}/kubectl" \
&& chmod +x /usr/local/bin/kubectl \
&& curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash

# Install Docker buildx for multi-platform image builds
RUN mkdir -p /usr/libexec/docker/cli-plugins && \
BUILDX_VERSION=$(curl -s https://api.github.com/repos/docker/buildx/releases/latest | grep tag_name | head -1 | awk -F'"' '{print $4}' | sed 's/^v//') && \
ARCH=$(dpkg --print-architecture | sed 's/arm64/arm64/; s/amd64/amd64/') && \
curl -fsSL "https://github.com/docker/buildx/releases/download/v${BUILDX_VERSION}/buildx-v${BUILDX_VERSION}.linux-${ARCH}" -o /usr/libexec/docker/cli-plugins/docker-buildx && \
chmod +x /usr/libexec/docker/cli-plugins/docker-buildx

# Install Docker Compose v2 as a Docker CLI plugin.
RUN mkdir -p /usr/libexec/docker/cli-plugins && \
    curl -fsSLo /tmp/docker-compose-release.json https://api.github.com/repos/docker/compose/releases/latest && \
    COMPOSE_VERSION="$(grep -m1 '"tag_name":' /tmp/docker-compose-release.json | sed -E 's/.*"v?([^"]+)".*/\1/')" && \
    { [ -n "$COMPOSE_VERSION" ] || { echo "ERROR: could not determine Docker Compose version" >&2; exit 1; }; } && \
    ARCH="$(dpkg --print-architecture)" && \
    case "$ARCH" in \
      amd64) COMPOSE_ARCH="x86_64" ;; \
      arm64) COMPOSE_ARCH="aarch64" ;; \
      *) echo "Unsupported Docker Compose architecture: $ARCH" >&2; exit 1 ;; \
    esac && \
    curl -fsSLo /usr/libexec/docker/cli-plugins/docker-compose \
      "https://github.com/docker/compose/releases/download/v${COMPOSE_VERSION}/docker-compose-linux-${COMPOSE_ARCH}" && \
    chmod +x /usr/libexec/docker/cli-plugins/docker-compose && \
    rm -f /tmp/docker-compose-release.json

# Keep the shared Rust toolchain/cache usable from the non-root dev user.
RUN chmod -R a+rwX "${CARGO_HOME}" "${RUSTUP_HOME}"

# Set working directory
WORKDIR /workspace
