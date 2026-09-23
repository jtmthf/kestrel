# `kestrel-dev`

The image Kestrel's own development runs in. It derives from [`kestrel-env`](../kestrel-env/README.md)
and keeps its supervisor entrypoint, user, and working directory. It adds what an agent needs to
work on this repository without setting anything up in its checkout:

| Tool | Pinned by |
| --- | --- |
| Rust 1.96.0 with `cargo`, `rustfmt`, and `clippy` | the `rust` base image digest, matching `rust-toolchain.toml` |
| `gcc` and `libc6-dev`, which link the workspace and compile its C dependencies | Debian's release on the day of the build |
| `git` | inherited from `kestrel-env` |
| `gh` | `GH_VERSION` and a SHA-256 per architecture |
| OpenCode 2 (`opencode acp`) | inherited from `kestrel-env` |
| Claude Code (`claude-agent-acp`) and Codex (`codex-acp`) | `package-lock.json`, installed with `npm ci` |

The two Node adapters bring the `node` binary with them, which the base image deliberately leaves
out ([ADR-0007](../../docs/adr/0007-acp-is-the-agent-runtime-contract.md)).

## Building it

Build `kestrel-env` first. Both builds use the repository root as the context:

```sh
docker build --file images/kestrel-env/Dockerfile --tag kestrel-env .
docker build --file images/kestrel-dev/Dockerfile --tag kestrel-dev .
```

`--build-arg KESTREL_ENV=<image>` derives it from a different base tag. The tests use it to build
on the `kestrel-env:test` image they built themselves.

To bump an adapter, change its exact version in `package.json`, then regenerate the lockfile in the
same Node image the build uses so the Linux optional dependencies are recorded:

```sh
docker run --rm --volume "$PWD/images/kestrel-dev:/w" --workdir /w \
  node@sha256:50c3b2f6988dfc307b86e5301d69611af31f4789bdf232863b07d3b02fe55ae0 \
  npm install --package-lock-only --ignore-scripts
```

To bump `gh`, change `GH_VERSION` along with both digests:

```sh
curl --location --silent "https://github.com/cli/cli/releases/download/v<version>/gh_<version>_linux_amd64.tar.gz" | sha256sum
```

## Running Kestrel's Runs in it

Point the control plane's `KESTREL_IMAGE` at it, for example with a gitignored
`compose.override.yaml`:

```yaml
services:
  kestrel:
    environment:
      KESTREL_IMAGE: kestrel-dev
```

Do not do this with `KESTREL_ENV_IMAGE`. Compose builds the `kestrel-env` Dockerfile under that
tag, so the base image would overwrite this one.

An Agent names its runtime as `opencode`, `claude` or `codex`, and the work role spawns
`opencode acp --print-logs`, `claude-agent-acp` or `codex-acp` for it.

## No credentials

Nothing in the image signs a runtime or `gh` in. The home directory contains only the skeleton
files `useradd` creates, and no variable in the image's environment names a key, token, or secret.
Credentials come in when the Run starts ([ADR-0010](../../docs/adr/0010-a-provider-credential-crosses-the-link-at-the-spawn.md)).
A Subscription Profile's files, such as `.codex/auth.json`, are written beneath `/home/kestrel`
for the length of one Run and removed when it ends
([ADR-0025](../../docs/adr/0025-subscription-profiles-are-personal.md)). An OpenCode Go or Zen
subscription is a Subscription Profile variable (`OPENCODE_API_KEY`) rather than a file, and an
opencode `auth.json` is a one-time seed into the runtime's database, not a login kestrel refreshes
([ADR-0026](../../docs/adr/0026-kestrel-carries-named-credentials-never-a-runtimes-store.md)). `gh`
reads `GH_TOKEN` from its own environment. The token's scope is the operator's and kestrel does not
narrow it: a `repo`-scoped token merges pull requests as well as opening them, so a human merge gate
needs a reviewer identity other than the token's ([USAGE.md](../../USAGE.md) covers it where the
credential is set).

## How it is checked

`crates/kestrel/tests/development.rs` holds ignored tests. CI runs them on every change: the
toolchain, `git`, and `gh` each run; each of the three runtimes answers an ACP `initialize`; and no
credentials are present. `kestrel_passes_its_own_checks_in_the_image` mounts the checkout read-only
and runs the same `fmt`, `clippy`, `build`, and `test` commands as CI's workspace job. Because it
compiles the workspace three times, it runs in the scheduled `Development image` workflow:

```sh
cargo test --locked --package kestrel --test development -- --ignored kestrel_passes_its_own_checks
```
