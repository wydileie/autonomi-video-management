# ── Stage 1: Build local Autonomi devnet tooling ─────────────────────────────
FROM rust:1.97-slim-bookworm AS builder

ARG AUTONOMI_ANT_SDK_REF=v0.10.0
ARG AUTONOMI_ANT_NODE_REF=v0.13.0

RUN apt-get update && apt-get install -y \
    build-essential \
    git \
    libssl-dev \
    liblzma-dev \
    perl \
    pkg-config \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

RUN git clone https://github.com/WithAutonomi/ant-sdk.git ant-sdk && \
    cd ant-sdk && \
    git checkout "$AUTONOMI_ANT_SDK_REF" && \
    cd antd && \
    cargo build --release && \
    cp target/release/antd /usr/local/bin/antd

RUN git clone https://github.com/WithAutonomi/ant-node.git ant-node && \
    cd ant-node && \
    git checkout "$AUTONOMI_ANT_NODE_REF" && \
    cargo build --release --bin ant-devnet && \
    cp target/release/ant-devnet /usr/local/bin/ant-devnet

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates/common/Cargo.toml crates/common/Cargo.toml
COPY crates/rust_admin/Cargo.toml crates/rust_admin/Cargo.toml
COPY crates/rust_stream/Cargo.toml crates/rust_stream/Cargo.toml
COPY crates/antd_service/Cargo.toml crates/antd_service/Cargo.toml
COPY crates/launcher_core/Cargo.toml crates/launcher_core/Cargo.toml
COPY crates/standalone_launcher/Cargo.toml crates/standalone_launcher/Cargo.toml
RUN mkdir -p crates/common/src crates/rust_admin/src crates/rust_stream/src crates/antd_service/src crates/launcher_core/src crates/standalone_launcher/src && \
    echo "" > crates/common/src/lib.rs && \
    echo "fn main(){}" > crates/rust_admin/src/main.rs && \
    echo "fn main(){}" > crates/rust_stream/src/main.rs && \
    echo "fn main(){}" > crates/antd_service/src/main.rs && \
    echo "" > crates/launcher_core/src/lib.rs && \
    echo "fn main(){}" > crates/standalone_launcher/src/main.rs && \
    cargo build --release -p antd

COPY crates/common/src crates/common/src
COPY crates/antd_service/src crates/antd_service/src
COPY crates/launcher_core/src crates/launcher_core/src
RUN touch crates/common/src/lib.rs crates/antd_service/src/main.rs crates/launcher_core/src/lib.rs && \
    cargo build --release -p antd && \
    cp target/release/antd /usr/local/bin/autvid-antd-gateway

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    bash \
    ca-certificates \
    curl \
    jq \
    liblzma5 \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Pinned: a live "latest release" lookup made builds unreproducible and
# subject to GitHub API rate limits. Bump deliberately.
ARG FOUNDRY_TAG=v1.4.1

RUN ARCH="$(dpkg --print-architecture)" && \
    case "$ARCH" in \
      amd64) FOUNDRY_ARCH="amd64" ;; \
      arm64) FOUNDRY_ARCH="arm64" ;; \
      *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;; \
    esac && \
    curl -fsSLo /tmp/foundry.tar.gz \
      "https://github.com/foundry-rs/foundry/releases/download/${FOUNDRY_TAG}/foundry_${FOUNDRY_TAG}_linux_${FOUNDRY_ARCH}.tar.gz" && \
    tar -xzf /tmp/foundry.tar.gz -C /usr/local/bin anvil forge cast chisel && \
    rm -f /tmp/foundry.tar.gz

COPY --from=builder /usr/local/bin/antd /usr/local/bin/antd
COPY --from=builder /usr/local/bin/ant-devnet /usr/local/bin/ant-devnet
COPY --from=builder /usr/local/bin/autvid-antd-gateway /usr/local/bin/autvid-antd-gateway
COPY deploy/autonomi_devnet/start-local-devnet.sh /usr/local/bin/start-local-devnet.sh

RUN mkdir -p /data && \
    useradd --system --uid 10001 --create-home --shell /usr/sbin/nologin appuser && \
    chown -R appuser:appuser /data /home/appuser && \
    chmod +x /usr/local/bin/start-local-devnet.sh

USER appuser

VOLUME ["/data"]
EXPOSE 8082 50051

ENTRYPOINT ["/usr/local/bin/start-local-devnet.sh"]
