# syntax=docker/dockerfile:1

# The second agent the conformance suite runs against is reached through an adapter that is a
# Node program, and the shipped image carries neither (ADR-0007), so it is built into a copy.

# node:24-trixie-slim
FROM node@sha256:50c3b2f6988dfc307b86e5301d69611af31f4789bdf232863b07d3b02fe55ae0 AS adapter

ARG CODEX_ACP_VERSION=1.10.0
RUN npm install --global "@agentclientprotocol/codex-acp@${CODEX_ACP_VERSION}"

FROM kestrel-env:test

USER root
# Merged rather than replaced, so what the shipped image already puts here stays.
COPY --from=adapter /usr/local/bin /usr/local/bin
COPY --from=adapter /usr/local/lib/node_modules /usr/local/lib/node_modules
USER kestrel
