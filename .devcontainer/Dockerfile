FROM mcr.microsoft.com/devcontainers/python:1-3.12-bookworm

ENV DEBIAN_FRONTEND=noninteractive
ENV VIRTUAL_ENV=/opt/venv
ENV CARGO_HOME=/usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup
ENV PATH="/opt/venv/bin:${PATH}:/opt/mssql-tools18/bin:${CARGO_HOME}/bin"

# Keep the Autonomi toolchain on a single ant-node/protocol line.
ARG AUTONOMI_ANT_SDK_REF=v0.12.1
ARG AUTONOMI_ANT_CLIENT_REF=ant-cli-v0.3.6
ARG AUTONOMI_ANT_NODE_REF=v0.18.1

# Install all system packages and tools in fewer layers
RUN rm -f /etc/apt/sources.list.d/yarn.list \
&& apt-get update && apt-get install -y --no-install-recommends \
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
liblzma-dev \
protobuf-compiler \
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
&& apt-get clean \
&& rm -rf /var/lib/apt/lists/* /var/cache/apt/*

# Install the Docker CLI only. The container talks to OrbStack through the
# mounted host socket, so it does not need a Docker daemon/containerd stack.
RUN mkdir -p -m 755 /etc/apt/keyrings \
&& curl -fsSL https://download.docker.com/linux/debian/gpg \
  > /etc/apt/keyrings/docker.asc \
&& chmod a+r /etc/apt/keyrings/docker.asc \
&& echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian bookworm stable" \
  > /etc/apt/sources.list.d/docker.list \
&& apt-get update \
&& apt-get install -y --no-install-recommends docker-ce-cli docker-buildx-plugin docker-compose-plugin \
&& apt-get clean \
&& rm -rf /var/lib/apt/lists/* /var/cache/apt/*

# Install GitHub CLI from the official apt repository so PR/review workflows
# are available after every devcontainer rebuild.
RUN mkdir -p -m 755 /etc/apt/keyrings \
&& curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
  > /etc/apt/keyrings/githubcli-archive-keyring.gpg \
&& chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg \
&& echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
  > /etc/apt/sources.list.d/github-cli.list \
&& apt-get update \
&& apt-get install -y --no-install-recommends gh \
&& apt-get clean \
&& rm -rf /var/lib/apt/lists/* /var/cache/apt/*

# Install Node.js 24 and the Python package runner used by MCP helpers.
RUN mkdir -p /etc/apt/keyrings \
&& curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg \
&& echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_24.x nodistro main" > /etc/apt/sources.list.d/nodesource.list \
&& apt-get update \
&& apt-get install -y --no-install-recommends nodejs=24.20.0-1nodesource1 \
&& pip install --no-cache-dir uv \
&& ln -s /usr/bin/fdfind /usr/local/bin/fd \
&& ln -s /usr/bin/batcat /usr/local/bin/bat \
&& apt-get clean \
&& rm -rf /var/lib/apt/lists/* /var/cache/apt/*

# Install Rust and Rust development tools
RUN mkdir -p "${CARGO_HOME}" "${RUSTUP_HOME}" \
&& chmod -R a+rwX "${CARGO_HOME}" "${RUSTUP_HOME}" \
&& curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain 1.98.1 \
&& rustup component add rustfmt clippy \
&& cargo install cargo-edit cargo-outdated \
&& chmod -R a+rwX "${CARGO_HOME}" "${RUSTUP_HOME}" \
&& rm -rf "${CARGO_HOME}/registry" "${CARGO_HOME}/git" /root/.cache /tmp/*

# Build and install antd (Autonomi daemon), the Autonomi MCP server, and the
# dev CLI support libraries from ant-sdk in a single layer to share the cargo cache.
RUN git clone https://github.com/WithAutonomi/ant-sdk.git && \
    cd ant-sdk && \
    git checkout "$AUTONOMI_ANT_SDK_REF" && \
    cd antd && \
    cargo build --locked --release && \
    cp target/release/antd /usr/local/bin/antd-upstream && \
    cd ../antd-py && \
    pip install --no-cache-dir ".[rest]" && \
    cd ../antd-mcp && \
    pip install --no-cache-dir . && \
    cd ../.. && \
    rm -rf ant-sdk "${CARGO_HOME}/registry" "${CARGO_HOME}/git" /root/.cache /tmp/*

# Build and install ant (Autonomi CLI)
RUN git clone https://github.com/WithAutonomi/ant-client.git && \
    cd ant-client && \
    git checkout "$AUTONOMI_ANT_CLIENT_REF" && \
    cargo build --locked --release --bin ant && \
    cp target/release/ant /usr/local/bin/ && \
    cd .. && \
    rm -rf ant-client "${CARGO_HOME}/registry" "${CARGO_HOME}/git" /root/.cache /tmp/*

# Build ant-devnet from ant-node — required for the local devnet used in development.
# ant dev start normally does this via `cargo run`; we pre-build it here so startup
# is instant and no source repos are needed at runtime.
RUN git clone https://github.com/WithAutonomi/ant-node.git && \
    cd ant-node && \
    git checkout "$AUTONOMI_ANT_NODE_REF" && \
    cargo build --locked --release --bin ant-devnet && \
    cp target/release/ant-devnet /usr/local/bin/ && \
    cd .. && \
    rm -rf ant-node "${CARGO_HOME}/registry" "${CARGO_HOME}/git" /root/.cache /tmp/*

ARG FOUNDRY_TAG=v1.8.1
# Install Foundry (anvil) — required by ant-devnet to run the local EVM chain.
# Downloads pre-built binaries directly from GitHub releases (more reliable in Docker
# than the foundryup installer script which can fail on rate limits or interactive prompts).
RUN ARCH=$(dpkg --print-architecture) && \
    case "$ARCH" in \
      amd64) FOUNDRY_ARCH="amd64"; FOUNDRY_SHA256="37b45855232e57624d90113b049ca54f0c92055bb5c1997fcbdc3076c7b89c10" ;; \
      arm64) FOUNDRY_ARCH="arm64"; FOUNDRY_SHA256="27a32bd282d73018ab4d043de15ab0320b561c71b4bf3a549b130a0806e79f5c" ;; \
      *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;; \
    esac && \
    curl -fsSLo /tmp/foundry.tar.gz \
      "https://github.com/foundry-rs/foundry/releases/download/${FOUNDRY_TAG}/foundry_${FOUNDRY_TAG}_linux_${FOUNDRY_ARCH}.tar.gz" && \
    echo "${FOUNDRY_SHA256}  /tmp/foundry.tar.gz" | sha256sum -c - && \
    tar -xzf /tmp/foundry.tar.gz -C /tmp anvil && \
    install -m 0755 /tmp/anvil /usr/local/bin/anvil && \
    rm -f /tmp/foundry.tar.gz /tmp/anvil && \
    anvil --version

# Create docker group and add vscode so the Docker socket (mounted from OrbStack
# at runtime as root:root 0660) becomes accessible after postStartCommand chgrps it.
RUN groupadd -f docker && usermod -aG docker vscode

# Optional infrastructure tools are outside the application toolchain.
# Enable explicitly and provide release pins; the core image does not need them.
ARG INSTALL_EXTRA_TOOLS=false
ARG KUBECTL_VERSION
ARG HELM_VERSION
ARG POWERSHELL_VERSION
RUN if [ "$INSTALL_EXTRA_TOOLS" = true ]; then \
      test -n "$KUBECTL_VERSION" && test -n "$HELM_VERSION" && test -n "$POWERSHELL_VERSION" && \
      ARCH="$(dpkg --print-architecture)" && \
      curl -fsSLo /usr/local/bin/kubectl "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${ARCH}/kubectl" && \
      curl -fsSLo /tmp/helm.tgz "https://get.helm.sh/helm-${HELM_VERSION}-linux-${ARCH}.tar.gz" && \
      tar -xzf /tmp/helm.tgz -C /tmp && install /tmp/linux-${ARCH}/helm /usr/local/bin/helm && \
      case "$ARCH" in amd64) PS_ARCH=x64 ;; arm64) PS_ARCH=arm64 ;; *) exit 1 ;; esac && \
      curl -fsSLo /tmp/powershell.tgz "https://github.com/PowerShell/PowerShell/releases/download/v${POWERSHELL_VERSION}/powershell-${POWERSHELL_VERSION}-linux-${PS_ARCH}.tar.gz" && \
      mkdir -p /opt/powershell && tar -xzf /tmp/powershell.tgz -C /opt/powershell && \
      chmod +x /opt/powershell/pwsh /usr/local/bin/kubectl && ln -s /opt/powershell/pwsh /usr/local/bin/pwsh && \
      rm -rf /tmp/helm.tgz /tmp/powershell.tgz /tmp/linux-${ARCH}; \
    fi

# Keep the shared Rust toolchain/cache discoverable from login shells.
RUN printf '%s\n' 'export PATH="/opt/venv/bin:/usr/local/python/current/bin:/usr/local/py-utils/bin:/usr/local/jupyter:/usr/local/share/nvm/current/bin:/usr/local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/mssql-tools18/bin:/usr/local/cargo/bin:$PATH"' \
  > /etc/profile.d/00-devcontainer-path.sh

# Set working directory
WORKDIR /workspace

# Refresh inherited development utilities as well as application dependencies.
# The Python devcontainer base includes optional formatter/tool virtualenvs.
RUN apt-get update && apt-get upgrade -y \
 && npm install -g npm@12.0.2 \
 && python3 -m pip install --no-cache-dir --upgrade pip==26.2.1 setuptools==84.0.0 wheel==0.48.0 GitPython==3.1.62 \
 && for python in /usr/local/py-utils/venvs/*/bin/python; do \
      [ ! -x "$python" ] || python3 -m pip --python "$python" install --no-cache-dir --upgrade pip==26.2.1 setuptools==84.0.0 wheel==0.48.0; \
    done \
 && python3 -m pip --python /usr/local/py-utils/venvs/black/bin/python install --no-cache-dir --upgrade black==26.5.1 \
 && npm cache clean --force \
 && apt-get clean && rm -rf /var/lib/apt/lists/* /var/cache/apt/* /root/.cache

# Patch npm's bundled dependency ranges without resolving its unpublished dev tools.
RUN node -e 'const fs=require("fs"); const p="/usr/lib/node_modules/npm/package.json"; const j=JSON.parse(fs.readFileSync(p)); delete j.devDependencies; fs.writeFileSync(p,JSON.stringify(j));' \
 && npm install --prefix /usr/lib/node_modules/npm --no-save --ignore-scripts --omit=dev \
      brace-expansion@5.0.9 ip-address@10.3.1 tar@7.5.21 \
 && npm cache clean --force

# The repository audit target must also work inside the headless bench.
RUN cargo install cargo-deny --version 0.20.2 --locked \
 && rm -rf "${CARGO_HOME}/registry" "${CARGO_HOME}/git" /root/.cache

RUN apt-get update && apt-get install -y --no-install-recommends sqlite3 \
 && apt-get clean && rm -rf /var/lib/apt/lists/* /var/cache/apt/*
