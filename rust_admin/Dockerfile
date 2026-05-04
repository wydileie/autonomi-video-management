# ── Stage 1: Build ────────────────────────────────────────────────────────────
FROM rust:1.95-slim-bookworm AS builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependencies before copying source
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
    cargo build --release -p rust_admin

COPY common/src common/src
COPY rust_admin/migrations rust_admin/migrations
COPY rust_admin/src rust_admin/src
RUN touch common/src/lib.rs rust_admin/src/main.rs && cargo build --release -p rust_admin

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    ffmpeg \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/rust_admin /usr/local/bin/rust_admin

RUN mkdir -p /var/lib/autvid/processing /catalog && \
    useradd --uid 1000 --home-dir /nonexistent --shell /usr/sbin/nologin appuser && \
    chown -R appuser:appuser /var/lib/autvid /catalog

USER appuser

EXPOSE 8000

CMD ["rust_admin"]
