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

# testcontainers ignores the docker CLI context, so point it at colima's socket
# when that is where the daemon lives.
colima_socket := home_directory() / ".colima/default/docker.sock"
export DOCKER_HOST := env("DOCKER_HOST", if path_exists(colima_socket) == "true" { "unix://" + colima_socket } else { "unix:///var/run/docker.sock" })

# Run tests in parallel with nextest; args pass through (`-p relayer`, `--profile ci`)
test *ARGS:
    cargo nextest run --all-features {{ARGS}}

# Run doctests, which nextest skips
test-doc:
    cargo test --workspace --all-features --doc

ci: fmt clippy test test-doc
