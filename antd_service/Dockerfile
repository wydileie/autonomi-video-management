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

COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN cargo build --release

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
