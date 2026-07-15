# ── Stage 1: Chef (shared toolchain + cargo-chef, layer-identical across services) ──
FROM rust:1.97-slim-bookworm AS chef

RUN apt-get update && apt-get install -y \
    build-essential \
    git \
    liblzma-dev \
    perl \
    pkg-config \
    libssl-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-chef --locked

WORKDIR /app

# ── Stage 2: Plan (recipe.json only changes when dependencies change) ─────────
FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 3: Build (dependency layer cached on recipe content) ────────────────
FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p rust_admin
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p rust_admin

# ── Stage 4: Runtime ──────────────────────────────────────────────────────────
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
