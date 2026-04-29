set shell := ["bash", "-cu"]

default:
    @just --list

fmt:
    cargo fmt --all -- --check

fmt-fix:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

build:
    cargo build --workspace --all-features

test:
    cargo test --workspace --all-features

ci: fmt clippy test
