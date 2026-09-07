# syntax=docker/dockerfile:1

# Derived from kestrel-env (images/kestrel-env/README.md, "Deriving from it"), which carries only
# the supervisor, opencode and git. A Run that has to open a pull request on the repository it was
# handed needs a way to call GitHub, and this is that way: nothing in the base image or the
# supervisor knows GitHub exists.
FROM kestrel-env AS github

USER root

ARG TARGETARCH
ARG GH_VERSION=2.100.0
ARG GH_SHA256_AMD64=e4d4bb4498e8d007abe545b6568926793ace1b6447da598294a610018cb164be
ARG GH_SHA256_ARM64=ea4e7a581a32ccad6cc7923cb1576ac5859ba4b9a16ab22eb8f8a96e78e2e961

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm --recursive --force /var/lib/apt/lists/*

# The release's own checksums file is a second network fetch to trust; the digest recorded here
# is what CI has to change when GH_VERSION moves, same as OPENCODE_SHA256_* above it in
# images/kestrel-env/Dockerfile.
RUN set -eu; \
    case "${TARGETARCH}" in \
    amd64) release=amd64; digest="${GH_SHA256_AMD64}" ;; \
    arm64) release=arm64; digest="${GH_SHA256_ARM64}" ;; \
    *) echo "kestrel-env-github is not built for ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    url="https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_linux_${release}.tar.gz"; \
    curl --fail --silent --show-error --location --output /tmp/gh.tar.gz "${url}"; \
    echo "${digest}  /tmp/gh.tar.gz" | sha256sum --check --strict -; \
    tar --extract --gzip --file /tmp/gh.tar.gz --strip-components=2 \
        --directory /usr/local/bin "gh_${GH_VERSION}_linux_${release}/bin/gh"; \
    rm /tmp/gh.tar.gz \
    && apt-get purge --yes curl \
    && apt-get autoremove --yes \
    && rm --recursive --force /var/lib/apt/lists/*

USER kestrel
