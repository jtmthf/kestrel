# `kestrel`

The control plane: one image, every role selected by argv
([ADR-0002](../../docs/adr/0002-two-deployables-the-environment-dials-out.md)). With no command it
starts every role in one process, which at `0.1` is the only supported topology.

It carries the `kestrel` binary and a `docker` client. The client is not a convenience: the default
`Compute` driver provisions an Environment by executing `docker`, so the work role in a container
is only as real as the client beside it
([ADR-0008](../../docs/adr/0008-the-control-plane-ships-dynamically-linked.md), which also records
why the binary is dynamically linked rather than a static musl build on `scratch`).

## Building it

The build context is the repository root, because the binary is built from source in it:

```sh
docker build --file images/kestrel/Dockerfile --tag kestrel .
```

Both base images are pinned by digest, the Rust toolchain by `rust-toolchain.toml`, and the crates
by `Cargo.lock` under `--locked`. The exception is apt, which resolves `ca-certificates` and
`docker-cli` to whatever the Debian release carries on the day — the one thing here that moves
without the Dockerfile changing.

CI writes the image's size and build time into every run's summary, and publishes
`ghcr.io/jtmthf/kestrel` from a clean checkout of `main`.

## Running one by hand

```sh
docker run --rm \
  --volume kestrel:/var/lib/kestrel \
  --volume /var/run/docker.sock:/var/run/docker.sock \
  --user root \
  --publish 7717:7717 \
  --env KESTREL_LINK=http://host.docker.internal:7717 \
  ghcr.io/jtmthf/kestrel
```

**The database is on the volume, and nothing else is.** `KESTREL_DATA_DIR` is `/var/lib/kestrel`,
kestrel creates and migrates its database there on first start, and replacing the container over
the same volume is the upgrade. A volume Docker initialises from the image comes up owned by the
unprivileged `kestrel` user; a host directory bind-mounted over that path does not, and has to be
made writable by that user first.

**`KESTREL_LINK` is not optional in a container.** The control plane tells an Environment where to
dial by handing it the address it bound, which inside a container is an address that reaches
nothing. Give it the name the Environment resolves the control plane by.

**The link listens on every interface here**, because what dials it is an Environment outside this
container ([ADR-0002](../../docs/adr/0002-two-deployables-the-environment-dials-out.md)). The
binary's own default is loopback, which is the right default for a binary on a laptop and the wrong
one in an image.

**Reaching the daemon by its socket costs the unprivileged user.** The image runs as `kestrel`, and
a bind-mounted `/var/run/docker.sock` is root's, so a by-hand run either joins that group or is
root — which is what `--user root` above is buying, and why it is not what a deployment should do.
A daemon reachable over TCP, named by `DOCKER_HOST`, needs neither — which is what
[`compose.yaml`](../../compose.yaml) does, through a proxy that forwards the requests the driver
makes and refuses the rest
([ADR-0009](../../docs/adr/0009-the-daemon-is-reached-through-a-filtered-proxy.md)).

## Roles

```sh
docker run --rm … ghcr.io/jtmthf/kestrel serve              # the API and the link
docker run --rm … ghcr.io/jtmthf/kestrel work               # claim queued Runs and execute them
docker run --rm … ghcr.io/jtmthf/kestrel organization list  # every other command is the CLI role
```

Splitting the roles across processes needs an out-of-process `Fanout` and `Timer`, which rung one
does not have; the argv seam exists so that the deployment shapes that split them later need no
second artifact.
