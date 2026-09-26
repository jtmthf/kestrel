# `kestrel-env`

The base image a **Run** executes in. Its Dockerfile is here so the image is derivable rather than
opaque ([ADR-0002](../../docs/adr/0002-two-deployables-the-environment-dials-out.md)).

It carries three things: the **supervisor**, **opencode**, and **git**. Nothing else.
[ADR-0007](../../docs/adr/0007-acp-is-the-agent-runtime-contract.md) took the `claude` binary out,
and Node left with it — opencode speaks ACP natively, so nothing stands between the supervisor and
the agent. A **Project**'s setup layers project dependencies on top; injecting the supervisor into
a bring-your-own image is the `0.7` escape hatch, and installing an agent from the ACP Registry is
`0.7` work under the constraints ADR-0007 records. Nothing here downloads an agent.

## Building it

The build context is the repository root, because the supervisor is built from source in it:

```sh
docker build --file images/kestrel-env/Dockerfile --tag kestrel-env .
```

Almost everything the build pulls in is pinned: both base images by digest, the Rust toolchain by
`rust-toolchain.toml`, the crates by `Cargo.lock` under `--locked`, and opencode by version and
SHA-256. The exception is apt, which resolves `git` and `ca-certificates` to whatever the Debian
release carries on the day — the one thing here that moves without the Dockerfile changing.

opencode 2 is a standalone build from
`https://opencode.ai/files/bin/<version>/opencode-linux-<arch>[-baseline].tar.gz`, not a GitHub
release. That channel publishes no checksums file, so a bump reads the SHA-256 from upstream's
update API, which maps each artifact to its URL and digest:

```sh
curl --location --silent "https://opencode.ai/update/api/latest/cli/opencode"
```

Take `.version` for `OPENCODE_VERSION`, and `.metadata.files["opencode-linux-x64-baseline.tar.gz"].sha256`
and `.metadata.files["opencode-linux-arm64.tar.gz"].sha256` for `OPENCODE_SHA256_AMD64` and
`OPENCODE_SHA256_ARM64`. x64 takes the `-baseline` artifact, which does not require AVX2, so a host
without it still runs; arm64 has no baseline variant and takes the default. Both are glibc builds,
matching the Debian base. The documentation page lags the files site, so read the version from the
update API rather than from it.

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
  --env KESTREL_AGENT_RUNTIME='opencode acp --print-logs' \
  --env KESTREL_AGENT_MODEL=<model, as the runtime advertises it> \
  kestrel-env
```

`KESTREL_AGENT_MODEL` is the model the Run's Agent named, set over ACP once the session is open; an
empty one leaves the runtime on its own default. `KESTREL_AGENT_AUTH` names the ACP authentication
method to log the agent in with, for a runtime that will not open a session until something has.

**No provider key is among them.** The **Provider Credentials** the Run's Organization holds arrive
over the link as the supervisor spawns the agent, and reach that process's environment and nothing
else ([ADR-0010](../../docs/adr/0010-a-provider-credential-crosses-the-link-at-the-spawn.md)). An
Environment run by hand this way reaches whatever the operator's own shell put in it, which is the
one thing the shipped path does not do.

The supervisor dials the link outward and the image exposes no port: an Environment needs egress and
nothing else, which is the capability every deployment target has.

It runs as the unprivileged `kestrel` user in `/workspace`, where a Project's repositories are
checked out side by side. An ACP session's working directory is the checkout of the first of them. Anything written into an Environment from
outside has to land owned by that user, and a derived image that installs packages needs `USER root`
first.

**Killing the supervisor ends the Run.** The agent is the supervisor's child over stdio, so nothing
in the image restarts one — the entrypoint is the supervisor itself, with no init or wrapper around
it. A supervisor that dies takes its Run with it: the lease expires unheld, and the Run ends failed
with an explicit exit status. That is the trade ADR-0007 records, and `crates/kestrel/tests/image.rs`
holds it to it.

## Deriving from it

A derived image adds what one kind of work needs and keeps the supervisor as its entrypoint.
[`images/kestrel-dev`](../kestrel-dev/README.md) is the one this repository ships. It adds Rust,
`gh`, and the Claude Code and Codex ACP adapters so Kestrel can work on itself. Those adapters are
Node programs, which is why they are in a derived image rather than this one. An adapter usually
advertises more than one way to sign in, and ACP gives a client no way to choose between them, so an
Agent on a derived image is configured with `--agent-auth` as well.
`crates/kestrel/tests/support/conformance-env.Dockerfile` is a smaller worked example. It builds the
conformance suite's second agent this way and drives it without any Kestrel code branching on it.
