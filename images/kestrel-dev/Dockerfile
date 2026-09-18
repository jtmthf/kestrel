# syntax=docker/dockerfile:1

ARG KESTREL_ENV=kestrel-env

# rust:1.96.0-slim-trixie, the toolchain rust-toolchain.toml names.
FROM rust@sha256:c37af730be4fd8104cbf9aedbd6ab259e51ca2d5437817a0f8680edf66ac6c28 AS rust

RUN rustup component add rustfmt clippy

# node:24-trixie-slim
FROM node@sha256:50c3b2f6988dfc307b86e5301d69611af31f4789bdf232863b07d3b02fe55ae0 AS adapters

WORKDIR /opt/acp
COPY images/kestrel-dev/package.json images/kestrel-dev/package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts

# debian:trixie-slim
FROM debian@sha256:d7e12182ce18b85b93007c1dedf31f2d29e01ccf3182cc4017c709b6259bc132 AS gh

ARG TARGETARCH
ARG GH_VERSION=2.100.0
ARG GH_SHA256_AMD64=e4d4bb4498e8d007abe545b6568926793ace1b6447da598294a610018cb164be
ARG GH_SHA256_ARM64=ea4e7a581a32ccad6cc7923cb1576ac5859ba4b9a16ab22eb8f8a96e78e2e961

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm --recursive --force /var/lib/apt/lists/*

RUN set -eu; \
    case "${TARGETARCH}" in \
    amd64) release=amd64; digest="${GH_SHA256_AMD64}" ;; \
    arm64) release=arm64; digest="${GH_SHA256_ARM64}" ;; \
    *) echo "kestrel-dev is not built for ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    url="https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_linux_${release}.tar.gz"; \
    curl --fail --silent --show-error --location --output /tmp/gh.tar.gz "${url}"; \
    echo "${digest}  /tmp/gh.tar.gz" | sha256sum --check --strict -; \
    tar --extract --gzip --file /tmp/gh.tar.gz --strip-components=2 \
        --directory /usr/local/bin "gh_${GH_VERSION}_linux_${release}/bin/gh"; \
    rm /tmp/gh.tar.gz

FROM ${KESTREL_ENV}

USER root

# The linker cargo drives, and the C compiler ring, aws-lc-sys and libsqlite3-sys build with.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends gcc libc6-dev \
    && rm --recursive --force /var/lib/apt/lists/*

COPY --from=gh /usr/local/bin/gh /usr/local/bin/gh
COPY --from=adapters /usr/local/bin/node /usr/local/bin/node
COPY --from=adapters /opt/acp /opt/acp
RUN ln --symbolic /opt/acp/node_modules/.bin/claude-agent-acp /usr/local/bin/claude-agent-acp \
    && ln --symbolic /opt/acp/node_modules/.bin/codex-acp /usr/local/bin/codex-acp

# Owned by the agent's user, because cargo writes its registry and git caches under CARGO_HOME.
COPY --from=rust --chown=kestrel:kestrel /usr/local/rustup /usr/local/rustup
COPY --from=rust --chown=kestrel:kestrel /usr/local/cargo /usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

USER kestrel
