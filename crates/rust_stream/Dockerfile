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
    cargo build --release -p rust_stream

COPY crates/common/src crates/common/src
COPY crates/launcher_core/src crates/launcher_core/src
COPY crates/rust_stream/src crates/rust_stream/src
RUN touch crates/common/src/lib.rs crates/launcher_core/src/lib.rs crates/rust_stream/src/main.rs && cargo build --release -p rust_stream

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /app/target/release/rust_stream /usr/local/bin/rust_stream

EXPOSE 8081

CMD ["/usr/local/bin/rust_stream"]
