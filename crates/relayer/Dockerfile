# syntax=docker/dockerfile:1.7
# Relayer image. Circuit artifacts (wasm + r1cs + zkey) are mounted at
# runtime from backend/stack/circuits/ → /circuits:ro (see docker-compose).
# Populate via `just -d backend/stack fetch-circuits`.
#
# Build context MUST be the repo root.
#
#   docker build -f backend/crates/relayer/Dockerfile -t lelantos/relayer:dev .

ARG RUST_VERSION=1.95
ARG DEBIAN_VERSION=bookworm

# ---------- chef base ----------
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS chef
RUN cargo install cargo-chef --locked --version ^0.1
WORKDIR /app
COPY backend/rust-toolchain.toml ./
RUN rustc --version && cargo --version

# ---------- planner ----------
FROM chef AS planner
COPY backend/ .
RUN cargo chef prepare --recipe-path recipe.json

# ---------- builder ----------
FROM chef AS builder
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends \
        libpq-dev \
        pkg-config \
        cmake \
    && rm -rf /var/lib/apt/lists/*

# ark-ff ships an asm Montgomery multiply for BN254 but gates it on
# `target_feature = "bmi2"` *and* `"adx"`, neither of which is in the baseline
# x86-64 target — so a plain `cargo build --release` compiles it out and every
# field multiply in the Groth16 MSMs and FFTs runs the generic CIOS loop.
# Enable exactly those two features — not a whole `target-cpu` level, which
# pulls in AVX2 and friends. The image then needs Haswell-era hardware (2013+).
# No equivalent gate exists on aarch64, so arm64 builds are left alone.
RUN mkdir -p /app/.cargo && \
    if [ "$TARGETARCH" = "amd64" ]; then \
        printf '[build]\nrustflags = ["-C", "target-feature=+bmi2,+adx"]\n' > /app/.cargo/config.toml; \
    fi

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --bin relayer

COPY backend/ .
RUN cargo build --release --bin relayer

# ---------- runtime ----------
FROM debian:${DEBIAN_VERSION}-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libpq5 \
        tini \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/relayer /usr/local/bin/relayer

RUN useradd --system --create-home --shell /bin/false app && chown -R app:app /app
USER app

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/relayer"]
