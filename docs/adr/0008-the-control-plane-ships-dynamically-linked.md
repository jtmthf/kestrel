# The control plane ships dynamically linked

The `kestrel` image is a dynamically linked binary on `debian:trixie-slim`. Rung `0.1` does not
ship a static musl build, and `FROM scratch` is not where the image is heading.

[ADR-0001](0001-rust-control-plane-typescript-supervisor.md) priced this and left it open: SQLite
reaches Rust through a C dependency, so a static musl build needs a C cross-toolchain and the
build pipeline gains a container — "`FROM scratch` survives". It does not.

**The Docker driver spends it.** The default `Compute` driver provisions an Environment by
executing `docker` ([ADR-0005](0005-six-ports-at-rung-one-are-named-boundaries.md)), so this image
carries a client binary whatever kestrel itself is linked against. A scratch image holding one
statically linked kestrel and nothing else cannot start a Run's Environment, which is the whole of
what the work role does. The reward the cross-toolchain was going to buy is already gone at rung
one.

**What wants a static binary is Lambda**, whose custom runtime takes a `bootstrap` executable
rather than an image. That is one target of eight, it arrives at `0.7`, and it is one of the two
targets where the work role splits into a deployment of its own anyway
([ADR-0002](0002-two-deployables-the-environment-dials-out.md)) — so the artifact it wants is not
this image with a different linker, and building one now would be building it blind.

**The deferral is a build flag, not a design.** Nothing in the control plane is glibc-specific:
there is no `dlopen`, no NSS lookup, and no C dependency but the SQLite `libsqlite3-sys` vendors
and `cc` compiles. What a static build adds is a musl target and a musl C compiler for the
architecture being built — ADR-0001's container, added to the pipeline on the day something needs
it rather than kept warm until then.

## Consequences

- **The image is Debian-sized rather than scratch-sized.** CI records its size and build time into
  every run's summary, so growth is visible in the run that causes it.
- **`0.7` owes two measurements, not a port.** musl's allocator and its resolver both behave
  differently from glibc's under a threaded SQLite workload and a polling HTTP client, and neither
  difference is knowable from here.
- **The published image is `linux/amd64`.** An arm64 host runs it emulated until either a native
  runner or a cross toolchain is worth carrying, which is the same toolchain question one
  architecture over.
