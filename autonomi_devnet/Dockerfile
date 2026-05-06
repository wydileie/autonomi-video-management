# ── Stage 1: Build local Autonomi devnet tooling ─────────────────────────────
FROM rust:1.95-slim-bookworm AS builder

ARG AUTONOMI_ANT_SDK_REF=v0.5.1
ARG AUTONOMI_ANT_NODE_REF=v0.11.0

RUN apt-get update && apt-get install -y \
    build-essential \
    git \
    libssl-dev \
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
COPY common/Cargo.toml common/Cargo.toml
COPY rust_admin/Cargo.toml rust_admin/Cargo.toml
COPY rust_stream/Cargo.toml rust_stream/Cargo.toml
COPY antd_service/Cargo.toml antd_service/Cargo.toml
RUN mkdir -p common/src rust_admin/src rust_stream/src antd_service/src && \
    echo "" > common/src/lib.rs && \
    echo "fn main(){}" > rust_admin/src/main.rs && \
    echo "fn main(){}" > rust_stream/src/main.rs && \
    echo "fn main(){}" > antd_service/src/main.rs && \
    cargo build --release -p antd

COPY common/src common/src
COPY antd_service/src antd_service/src
RUN touch common/src/lib.rs antd_service/src/main.rs && \
    cargo build --release -p antd && \
    cp target/release/antd /usr/local/bin/autvid-antd-gateway

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    bash \
    ca-certificates \
    curl \
    jq \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

RUN ARCH="$(dpkg --print-architecture)" && \
    case "$ARCH" in \
      amd64) FOUNDRY_ARCH="amd64" ;; \
      arm64) FOUNDRY_ARCH="arm64" ;; \
      *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;; \
    esac && \
    FOUNDRY_TAG="$(curl -fsSL https://api.github.com/repos/foundry-rs/foundry/releases/latest | jq -r '.tag_name')" && \
    curl -fsSLo /tmp/foundry.tar.gz \
      "https://github.com/foundry-rs/foundry/releases/download/${FOUNDRY_TAG}/foundry_${FOUNDRY_TAG}_linux_${FOUNDRY_ARCH}.tar.gz" && \
    tar -xzf /tmp/foundry.tar.gz -C /usr/local/bin anvil forge cast chisel && \
    rm -f /tmp/foundry.tar.gz

COPY --from=builder /usr/local/bin/antd /usr/local/bin/antd
COPY --from=builder /usr/local/bin/ant-devnet /usr/local/bin/ant-devnet
COPY --from=builder /usr/local/bin/autvid-antd-gateway /usr/local/bin/autvid-antd-gateway
COPY autonomi_devnet/start-local-devnet.sh /usr/local/bin/start-local-devnet.sh

RUN mkdir -p /data && \
    useradd --system --uid 10001 --create-home --shell /usr/sbin/nologin appuser && \
    chown -R appuser:appuser /data /home/appuser && \
    chmod +x /usr/local/bin/start-local-devnet.sh

USER appuser

VOLUME ["/data"]
EXPOSE 8082 50051

ENTRYPOINT ["/usr/local/bin/start-local-devnet.sh"]
