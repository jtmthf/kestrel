# Cloudflare runs kestrel as a container, and forfeits hibernation

On Cloudflare, kestrel is the same container image it is everywhere else, behind a Worker that routes
and nothing more. kestrel does **not** run Worker-native, and gives up Durable Object hibernation to
keep it that way.

**Ports and adapters do not rescue a Worker-native control plane, in any language.** They abstract
what code *calls*, not what runtime it *executes in*. Worker-native means running inside workerd,
which executes JavaScript and WASM — a Rust binary does not run there at all, and `workers-rs` lags
the JavaScript surface (no container type in `worker-sys/src/types`). And even in TypeScript,
Worker-native is not the same program with different adapters: there is no long-lived process, no
threads, invocations are request-scoped, and the only way to hold state and a socket is a Durable
Object. A control plane written as a server does not become a Worker by swapping six adapters — it
gets restructured around Durable Objects, and then the portability is nominal. That is the weld
[#5](https://github.com/jtmthf/kestrel/issues/5) found in Open-Inspect and the thing
[`README.md`](../../README.md) sells against.

The container path is where the six ports actually pay off on Cloudflare: the same binary runs, and
`Store` binds to Durable Object SQLite or D1, `Fanout` to the Durable Object itself, `Timer` to a DO
alarm plus a Cron Trigger. **Ports below the container, not instead of it.**

**Hibernation goes because keeping it would fork the collaboration layer.** Hibernation requires the
Durable Object itself to call `acceptWebSocket`; forwarding the socket through to the container gives
it up. Keeping it would put `Fanout`, connection state and cursor logic in the DO — a second
implementation of a layer [#15](https://github.com/jtmthf/kestrel/issues/15) settled is
kestrel-owned and *not* a pluggable seam, and the tiering
[#10](https://github.com/jtmthf/kestrel/issues/10) refused.
[#14](https://github.com/jtmthf/kestrel/issues/14) already designed the uniform promise to Container
Apps' 240-second ceiling and ruled that *the connection is never the unit of session continuity*, so
hibernation is an optimisation on a substrate-specific mechanism kestrel's contract deliberately does
not depend on.

## The principle this generalises to

**kestrel ships native packaging wherever the native unit is the same artifact differently wrapped,
and a container wherever native packaging would mean a different program.**

- **Lambda passes.** A static binary as `bootstrap` on the `provided.al2023` custom runtime, in a
  zip. Same artifact, different wrapper — no `aws-lambda-web-adapter`, no 10 GB image, faster cold
  start. This is the *preferred* Lambda deployment, with the container image as the alternative.
- **Cloudflare fails.** Worker-native is a different program.
- **Vercel-native** is a stated goal for `0.7`, not a constraint on any decision taken here.

## Costs, stated

- **On Cloudflare an idle connected session costs a running container**, where Open-Inspect's costs
  nothing. That is the price of the portability kestrel exists for, and it is worse than the
  incumbent on exactly one axis on exactly one substrate.
- **GCP and Azure have no GA first-party Rust SDK.** `Compute` for Cloud Run jobs and Container Apps
  dynamic sessions will be hand-rolled or generated REST at `0.7`. Known, accepted, lower priority.

## Open, and deliberately not blocking

Whether a WebSocket upgrade forwards cleanly through `containerInstance.fetch` on Cloudflare, and
whether a `Dockerfile.vercel` container receives upgrades on Vercel, are both unsourced. Neither
binds the supervisor link, which is SSE over plain HTTP
([ADR-0002](0002-two-deployables-the-environment-dials-out.md)). They bind the `0.3` multiplayer
transport for human clients, and belong to that rung.
