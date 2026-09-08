# syntax=docker/dockerfile:1
# ── Stage 1: Build local Autonomi devnet tooling ─────────────────────────────
FROM rust:1.98.0-slim-bookworm@sha256:1469a27c125cb5a3aebfa4f4e4665d935b02fb72cc093b2c974b3d740e43f157 AS builder
# Official Docker images lag the patch release; install the pinned toolchain explicitly.
RUN rustup toolchain install 1.98.1 --profile minimal && rustup default 1.98.1 && rustup toolchain uninstall 1.98.0

ARG AUTONOMI_ANT_NODE_REF=v0.18.1

RUN apt-get update && apt-get upgrade -y && apt-get install -y --no-install-recommends \
    build-essential \
    git \
    libssl-dev \
    liblzma-dev \
    perl \
    pkg-config \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

RUN git clone https://github.com/WithAutonomi/ant-node.git ant-node && \
    cd ant-node && \
    git checkout "$AUTONOMI_ANT_NODE_REF" && \
    cargo build --locked --release --bin ant-devnet && \
    cp target/release/ant-devnet /usr/local/bin/ant-devnet

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY vendor/ vendor/
ARG TARGETARCH
RUN --mount=type=cache,id=autvid-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=autvid-rust-${TARGETARCH},target=/app/target,sharing=locked \
    cargo build --locked --release -p antd && cp target/release/antd /usr/local/bin/autvid-antd-gateway

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get upgrade -y && apt-get install -y --no-install-recommends \
    bash \
    ca-certificates \
    curl \
    jq \
    liblzma5 \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Pinned: a live "latest release" lookup made builds unreproducible and
# subject to GitHub API rate limits. Bump deliberately.
ARG FOUNDRY_TAG=v1.8.1

RUN ARCH="$(dpkg --print-architecture)" && \
    case "$ARCH" in \
      amd64) FOUNDRY_ARCH="amd64"; FOUNDRY_SHA256="37b45855232e57624d90113b049ca54f0c92055bb5c1997fcbdc3076c7b89c10" ;; \
      arm64) FOUNDRY_ARCH="arm64"; FOUNDRY_SHA256="27a32bd282d73018ab4d043de15ab0320b561c71b4bf3a549b130a0806e79f5c" ;; \
      *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;; \
    esac && \
    curl -fsSLo /tmp/foundry.tar.gz \
      "https://github.com/foundry-rs/foundry/releases/download/${FOUNDRY_TAG}/foundry_${FOUNDRY_TAG}_linux_${FOUNDRY_ARCH}.tar.gz" && \
    echo "${FOUNDRY_SHA256}  /tmp/foundry.tar.gz" | sha256sum -c - && \
    tar -xzf /tmp/foundry.tar.gz -C /usr/local/bin anvil forge cast chisel && \
    rm -f /tmp/foundry.tar.gz

COPY --from=builder /usr/local/bin/ant-devnet /usr/local/bin/ant-devnet
COPY --from=builder /usr/local/bin/autvid-antd-gateway /usr/local/bin/autvid-antd-gateway
COPY deploy/autonomi_devnet/start-local-devnet.sh /usr/local/bin/start-local-devnet.sh

RUN mkdir -p /data && \
    useradd --system --uid 10001 --create-home --shell /usr/sbin/nologin appuser && \
    chown -R appuser:appuser /data /home/appuser && \
    chmod +x /usr/local/bin/start-local-devnet.sh

USER appuser

VOLUME ["/data"]
EXPOSE 8082

ENTRYPOINT ["/usr/local/bin/start-local-devnet.sh"]
