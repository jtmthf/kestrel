# syntax=docker/dockerfile:1

# The scripted ACP agent is a test artifact, so it is built into a copy of `kestrel-env` rather
# than into the image itself.

# rust:1.96.0-slim-trixie
FROM rust@sha256:c37af730be4fd8104cbf9aedbd6ab259e51ca2d5437817a0f8680edf66ac6c28 AS scripted-agent

WORKDIR /src
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release --package kestrel-scripted-agent

FROM kestrel-env:test

COPY --from=scripted-agent /src/target/release/kestrel-scripted-agent /usr/local/bin/kestrel-scripted-agent
