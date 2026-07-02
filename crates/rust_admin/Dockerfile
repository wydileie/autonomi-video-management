# ── Stage 1: Build ────────────────────────────────────────────────────────────
FROM rust:1.96-slim-bookworm AS builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependencies before copying source
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
    cargo build --release -p rust_admin

COPY crates/common/src crates/common/src
COPY crates/launcher_core/src crates/launcher_core/src
COPY crates/rust_admin/migrations crates/rust_admin/migrations
COPY crates/rust_admin/src crates/rust_admin/src
RUN touch crates/common/src/lib.rs crates/launcher_core/src/lib.rs crates/rust_admin/src/main.rs && cargo build --release -p rust_admin

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get upgrade -y && apt-get install -y \
    ca-certificates \
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
