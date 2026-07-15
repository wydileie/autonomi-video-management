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
RUN cargo chef cook --release --recipe-path recipe.json -p antd
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p antd

# ── Stage 4: Runtime ──────────────────────────────────────────────────
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
