# ── Stage 1: Build antd from source ──────────────────────────────────────────
FROM rust:1.95-slim-bookworm AS builder

# Match the devcontainer's antd build so local and containerized runs use the
# same saorsa-core wire protocol.
ARG AUTONOMI_ANT_SDK_REF=2ed9b14bda42fbdc604a173cc1af27be0964908f

RUN apt-get update && apt-get install -y \
    build-essential \
    git \
    libssl-dev \
    pkg-config \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Clone and build only the antd binary; ant-core is fetched via Cargo.
RUN git clone https://github.com/WithAutonomi/ant-sdk.git . && \
    git checkout "$AUTONOMI_ANT_SDK_REF" && \
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
