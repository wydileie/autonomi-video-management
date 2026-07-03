# ── Stage 1: Chef (shared toolchain + cargo-chef, layer-identical across services) ──
FROM rust:1.96-slim-bookworm AS chef

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
RUN cargo chef cook --release --recipe-path recipe.json -p rust_stream
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p rust_stream

# ── Stage 4: Runtime ──────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /app/target/release/rust_stream /usr/local/bin/rust_stream

EXPOSE 8081

CMD ["/usr/local/bin/rust_stream"]
