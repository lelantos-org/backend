# syntax=docker/dockerfile:1.7
# Multi-stage build using cargo-chef. Builds a single binary selected by
# ARG PACKAGE. Build context MUST be the backend root.
#
#   docker build -f Dockerfile --build-arg PACKAGE=ingester .

ARG RUST_VERSION=1.95
ARG DEBIAN_VERSION=bookworm

# ---------- chef base ----------
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS chef
RUN cargo install cargo-chef --locked --version ^0.1
WORKDIR /app
COPY rust-toolchain.toml ./
RUN rustc --version && cargo --version

# ---------- planner ----------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---------- builder ----------
FROM chef AS builder
ARG PACKAGE
RUN apt-get update && apt-get install -y --no-install-recommends \
        libpq-dev \
        pkg-config \
        cmake \
    && rm -rf /var/lib/apt/lists/*

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --bin "${PACKAGE}"

COPY . .
RUN cargo build --release --bin "${PACKAGE}"
RUN cp "/app/target/release/${PACKAGE}" /app/bin

# ---------- runtime ----------
FROM debian:${DEBIAN_VERSION}-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libpq5 \
        tini \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/bin /usr/local/bin/app

RUN useradd --system --create-home --shell /bin/false app
USER app

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/app"]
