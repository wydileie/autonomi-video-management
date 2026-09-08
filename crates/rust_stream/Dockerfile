# syntax=docker/dockerfile:1
FROM rust:1.98.0-slim-bookworm@sha256:1469a27c125cb5a3aebfa4f4e4665d935b02fb72cc093b2c974b3d740e43f157 AS builder
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
    cargo build --locked --release -p rust_stream && mkdir -p /out && cp /app/target/release/rust_stream /out/

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /out/rust_stream /usr/local/bin/rust_stream

EXPOSE 8081

CMD ["/usr/local/bin/rust_stream"]
