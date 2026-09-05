# syntax=docker/dockerfile:1

# debian:trixie-slim
FROM debian@sha256:d7e12182ce18b85b93007c1dedf31f2d29e01ccf3182cc4017c709b6259bc132 AS opencode

ARG TARGETARCH
ARG OPENCODE_VERSION=1.18.29
ARG OPENCODE_SHA256_AMD64=ea800b7ff56226b70952126c9fc1e2517ca4c4b5682fd9d3f9e87449697a1194
ARG OPENCODE_SHA256_ARM64=70baf769395ca4e7a68924026530c390eace194f3b7e4919d4efcb2aa2eed3c0

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm --recursive --force /var/lib/apt/lists/*

# The release publishes no checksums of its own, so one is recorded here and the build fails
# rather than shipping whatever the CDN served.
RUN set -eu; \
    case "${TARGETARCH}" in \
    amd64) release=x64; digest="${OPENCODE_SHA256_AMD64}" ;; \
    arm64) release=arm64; digest="${OPENCODE_SHA256_ARM64}" ;; \
    *) echo "kestrel-env is not built for ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    url="https://github.com/sst/opencode/releases/download/v${OPENCODE_VERSION}/opencode-linux-${release}.tar.gz"; \
    curl --fail --silent --show-error --location --output /tmp/opencode.tar.gz "${url}"; \
    echo "${digest}  /tmp/opencode.tar.gz" | sha256sum --check --strict -; \
    tar --extract --gzip --file /tmp/opencode.tar.gz --directory /usr/local/bin opencode; \
    rm /tmp/opencode.tar.gz

# rust:1.96.0-slim-trixie
FROM rust@sha256:c37af730be4fd8104cbf9aedbd6ab259e51ca2d5437817a0f8680edf66ac6c28 AS supervisor

WORKDIR /src
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release --package kestrel-supervisor

# debian:trixie-slim
FROM debian@sha256:d7e12182ce18b85b93007c1dedf31f2d29e01ccf3182cc4017c709b6259bc132

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates git \
    && rm --recursive --force /var/lib/apt/lists/* \
    && useradd --create-home kestrel \
    && install --directory --owner kestrel --group kestrel /workspace

COPY --from=opencode /usr/local/bin/opencode /usr/local/bin/opencode
COPY --from=supervisor /src/target/release/kestrel-supervisor /usr/local/bin/kestrel-supervisor

# Docker leaves HOME unset for anyone but root, and an agent runtime with nowhere to keep its
# state writes to `/`.
ENV HOME=/home/kestrel
USER kestrel
WORKDIR /workspace

# The Run's process, not a process that starts the Run's process: nothing here outlives the
# supervisor to restart it (ADR-0007).
ENTRYPOINT ["kestrel-supervisor"]
