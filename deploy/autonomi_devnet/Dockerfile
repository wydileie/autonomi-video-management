# syntax=docker/dockerfile:1
# ── Stage 1: Build local Autonomi devnet tooling ─────────────────────────────
FROM rust:1.99.0-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS builder
# Official Docker images lag the patch release; install the pinned toolchain explicitly.
RUN rustup toolchain install 1.99.1 --profile minimal && rustup default 1.99.1 && rustup toolchain uninstall 1.99.0

ARG AUTONOMI_ANT_NODE_REF=v0.23.1

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
ARG FOUNDRY_TAG=v1.8.6

RUN ARCH="$(dpkg --print-architecture)" && \
    case "$ARCH" in \
      amd64) FOUNDRY_ARCH="amd64"; FOUNDRY_SHA256="58850106984038f3a7200f38a4182c8cb423d3a4f4c7848eacbc2453e6bc35fc" ;; \
      arm64) FOUNDRY_ARCH="arm64"; FOUNDRY_SHA256="ca61f3f6e7d635f34e3e174409964d29126fa135794cd84cead8919dd50e0930" ;; \
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
