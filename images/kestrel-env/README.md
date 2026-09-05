# `kestrel-env`

The base image a **Run** executes in. Its Dockerfile is here so the image is derivable rather than
opaque ([ADR-0002](../../docs/adr/0002-two-deployables-the-environment-dials-out.md)).

It carries three things: the **supervisor**, **opencode**, and **git**. Nothing else.
[ADR-0007](../../docs/adr/0007-acp-is-the-agent-runtime-contract.md) took the `claude` binary out,
and Node left with it — opencode speaks ACP natively, so nothing stands between the supervisor and
the agent. A **Workspace**'s setup layers project dependencies on top; injecting the supervisor into
a bring-your-own image is the `0.7` escape hatch, and installing an agent from the ACP Registry is
`0.7` work under the constraints ADR-0007 records. Nothing here downloads an agent.

## Building it

The build context is the repository root, because the supervisor is built from source in it:

```sh
docker build --file images/kestrel-env/Dockerfile --tag kestrel-env .
```

Everything the build pulls in is pinned: both base images by digest, the Rust toolchain by
`rust-toolchain.toml`, the crates by `Cargo.lock` under `--locked`, and opencode by version and
SHA-256. The release carries no checksums of its own, so bumping `OPENCODE_VERSION` means bumping
`OPENCODE_SHA256_AMD64` and `OPENCODE_SHA256_ARM64` beside it:

```sh
curl --location --silent "https://github.com/sst/opencode/releases/download/v<version>/opencode-linux-x64.tar.gz" | sha256sum
```

The x64 build opencode publishes by default requires AVX2. A host without it wants the `-baseline`
release, which is a change of one word in the download URL.

CI writes the image's size and build time into every run's summary, so growth is visible in the run
that causes it. The opencode binary is nearly all of the size.

## Running one by hand

A Run's supervisor is the image's entrypoint, and it needs the control plane's address, the Run it is
executing, that Run's credential, and the command to spawn as the agent runtime.

```sh
docker run --rm \
  --add-host host.docker.internal:host-gateway \
  --env KESTREL_LINK=http://host.docker.internal:7717 \
  --env KESTREL_RUN=<run> \
  --env KESTREL_RUN_CREDENTIAL=<credential> \
  --env KESTREL_AGENT_RUNTIME='opencode acp' \
  kestrel-env
```

The supervisor dials the link outward and the image exposes no port: an Environment needs egress and
nothing else, which is the capability every deployment target has.

**Killing the supervisor ends the Run.** The agent is the supervisor's child over stdio, so nothing
in the image restarts one — the entrypoint is the supervisor itself, with no init or wrapper around
it. A supervisor that dies takes its Run with it: the lease expires unheld, and the Run ends failed
with an explicit exit status. That is the trade ADR-0007 records, and `crates/kestrel/tests/image.rs`
holds it to it.

## Deriving from it

Claude Code is reached through Zed's `claude-code-acp` adapter, which is Node, which is why it is a
derived image rather than the default. It is documented here and built nowhere:

```dockerfile
FROM kestrel-env

USER root
RUN apt-get update \
    && apt-get install --yes --no-install-recommends nodejs npm \
    && rm --recursive --force /var/lib/apt/lists/* \
    && npm install --global @zed-industries/claude-code-acp
USER kestrel
```

The Agent that runs in it names `claude-code-acp` as its runtime, and the supervisor spawns that
instead of `opencode acp`. The provider key reaches the agent through the spawn rather than the
protocol, so nothing about the derived image changes how a credential travels.
