# Rust control plane, TypeScript supervisor

> **Superseded by [ADR-0007](0007-acp-is-the-agent-runtime-contract.md).** The claim below that
> `canUseTool` is the only surface carrying a mid-run approval round-trip is false: ACP's
> `session/request_permission` carries it, and the supervisor is Rust.

kestrel is two programs in two languages. The control plane — every domain concept, all six ports,
the whole session model — is Rust. The supervisor that runs inside an `Environment` alongside the
agent runtime is TypeScript. The boundary is not a compromise between tastes: it is exactly the line
between kestrel's domain and someone else's library.

**The supervisor is TypeScript because of the Claude Agent SDK.** It is published as a library for
Python and TypeScript only, and Anthropic's documented alternative for other languages — `claude -p`
— has no mid-run approval callback: its permission surface is `--allowedTools`, `--permission-mode`
and `dontAsk`, which pre-authorise or deny. [`CONTEXT.md`](../../CONTEXT.md) defines an **Approval**
as a decision that *blocks a run until a human resolves it*, so a runtime that can only be
pre-authorised cannot participate in kestrel's governance at all. The SDK's `canUseTool` is the only
surface that carries that round-trip.

**The control plane is Rust because the domain is a pile of invariants.**
[`CONTEXT.md`](../../CONTEXT.md) states around twenty-five of them and a state machine on nearly
every noun — `open | sealed`, `queued | active | failed | unreachable`, `allow | deny | judge |
human`. Sum types with exhaustive matching, private constructors returning `Result`, and the absence
of null or a zero value make most of those unrepresentable rather than merely tested.

**The invariant that keeps the split honest: the supervisor never makes a decision the domain could
make.** It forwards approvals and blocks; it forwards hook callbacks and relays the verdict; it
holds a cursor and heartbeats. If a branch ever appears in the supervisor that reads domain state,
the split has failed, and it will be visible that day rather than a year later.

## What are *not* the reasons

Both of the obvious arguments for TypeScript were tested and rejected, and a future reader should
not reconstruct them:

- **opencode does not force TypeScript.** Its "embedded" SDK is the same generated network client
  with `fetch` swapped for an in-memory router, over an HTTP/REST + SSE surface described by a
  published OpenAPI 3.1 document. A non-TypeScript caller reimplements nothing. See
  [`docs/research/language-and-runtime.md`](../research/language-and-runtime.md) on branch
  `research/language-and-runtime`.
- **Cloudflare does not force TypeScript.** See [ADR-0006](0006-cloudflare-runs-as-a-container.md).

## Consequences

- **GCP and Azure have no GA first-party Rust SDK.** `Compute` for Cloud Run jobs and Container Apps
  dynamic sessions will be hand-rolled or generated REST at `0.7`. Known, accepted, lower priority.
- **SQLite reaches Rust through a C dependency**, so a static musl build needs a C cross-toolchain in
  CI. `FROM scratch` survives; the build pipeline gains a container.
- **This repo carries a TypeScript toolchain either way**, because the `0.3` web UI is TypeScript
  regardless. The split decides where the boundary sits, not whether there is a second language.
