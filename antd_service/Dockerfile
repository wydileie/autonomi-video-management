# ── Stage 1: Build antd from source ──────────────────────────────────────────
FROM rust:1.86-slim-bookworm AS builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    protobuf-compiler \
    git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Clone and build only the antd binary; ant-core is fetched via Cargo
RUN git clone --depth 1 https://github.com/WithAutonomi/ant-sdk.git . && \
    cd antd && \
    cargo build --release && \
    cp target/release/antd /usr/local/bin/antd

# ── Stage 2: Minimal runtime ──────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/antd /usr/local/bin/antd

# REST on 8082, gRPC on 50051
EXPOSE 8082 50051

ENTRYPOINT ["antd"]
