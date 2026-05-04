# ── Stage 1: Build the Autonomi 2.0 compatibility gateway ────────────────────
FROM rust:1.95-slim-bookworm AS builder

RUN apt-get update && apt-get install -y \
    build-essential \
    git \
    libssl-dev \
    pkg-config \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

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
RUN touch common/src/lib.rs antd_service/src/main.rs && cargo build --release -p antd

# ── Stage 2: Minimal runtime ──────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/antd /usr/local/bin/antd

RUN useradd --system --uid 10001 --create-home --shell /usr/sbin/nologin appuser
USER appuser

EXPOSE 8082

ENTRYPOINT ["antd"]
