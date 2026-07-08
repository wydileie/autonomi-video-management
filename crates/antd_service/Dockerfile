# ── Stage 1: Build the Autonomi 2.0 compatibility gateway ────────────────────
FROM rust:1.96-slim-bookworm AS builder

RUN apt-get update && apt-get install -y \
    build-essential \
    git \
    libssl-dev \
    liblzma-dev \
    perl \
    pkg-config \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

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
COPY crates/launcher_core/src crates/launcher_core/src
COPY crates/antd_service/src crates/antd_service/src
RUN touch crates/common/src/lib.rs crates/launcher_core/src/lib.rs crates/antd_service/src/main.rs && cargo build --release -p antd

# ── Stage 2: Minimal runtime ──────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get upgrade -y && apt-get install -y \
    ca-certificates \
    liblzma5 \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/antd /usr/local/bin/antd

RUN useradd --system --uid 10001 --create-home --shell /usr/sbin/nologin appuser
USER appuser

EXPOSE 8082

ENTRYPOINT ["antd"]
