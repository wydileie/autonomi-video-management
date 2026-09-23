# syntax=docker/dockerfile:1
FROM rust:1.98.1-slim-bookworm@sha256:ff521445a372125ed4f76e1453a1f8098f2d05332d1601d30db1c1f62757e730 AS builder
# Official Docker images lag the patch release; install the pinned toolchain explicitly.
RUN rustup toolchain install 1.98.1 --profile minimal && rustup default 1.98.1 && rustup toolchain uninstall 1.98.0
RUN apt-get update && apt-get install -y --no-install-recommends build-essential git liblzma-dev perl pkg-config libssl-dev protobuf-compiler && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY vendor/ vendor/
ARG TARGETARCH
# Share cached dependencies across service builds while keeping source patches real.
RUN --mount=type=cache,id=autvid-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=autvid-rust-${TARGETARCH},target=/app/target,sharing=locked \
    cargo build --locked --release -p rust_admin && mkdir -p /out && cp /app/target/release/rust_admin /out/

FROM debian:bookworm-slim

RUN apt-get update && apt-get upgrade -y && apt-get install -y \
    ca-certificates \
    ffmpeg \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /out/rust_admin /usr/local/bin/rust_admin

RUN mkdir -p /var/lib/autvid/processing /catalog && \
    useradd --uid 1000 --home-dir /nonexistent --shell /usr/sbin/nologin appuser && \
    chown -R appuser:appuser /var/lib/autvid /catalog

USER appuser

EXPOSE 8000

CMD ["rust_admin"]
