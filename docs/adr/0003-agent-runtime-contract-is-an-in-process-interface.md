# The agent-runtime contract is an in-process interface

kestrel drives an agent runtime through an interface **inside the supervisor**, not through a wire
protocol. One supervisor hosts every driver. A driver is thin: start the runtime, send a prompt,
stream events out, map permission requests, map exit status. Everything else — the outbound
connection, the lease heartbeat, reconnect-with-cursor, credential handling — is kestrel's, written
once, behind the driver.

The process boundary that matters is control-plane ↔ environment, and
[ADR-0002](0002-two-deployables-the-environment-dials-out.md) settles it. A second process boundary
at the runtime seam buys nothing.

**Why not make the wire protocol the contract.** v1's gate is the agent-runtime contract proven
against a second runtime, and [`README.md`](../../README.md) says a contract with a single
implementation is an untested abstraction. If the contract were the supervisor ↔ control-plane link,
a second author would have to satisfy the lease, the cursor semantics, the at-least-once replay and
the `ask` round-trip. Heavy contracts do not get second implementations, so that choice would make
the v1 gate harder to clear by construction.

**Why one supervisor rather than a driver per language.** opencode is pure REST over a published
spec, so a Rust driver in the control plane would be trivial and would keep TypeScript out of
`kestrel-env` on the default path. Rejected: the default runtime and the second runtime would then
sit on opposite sides of a language boundary and be driven through *two* contracts rather than one,
and the v1 gate would prove nothing.

## The two runtimes' shapes, recorded because they are the reason

- **opencode** — HTTP/REST + SSE described by a published OpenAPI 3.1 document; HTTP Basic auth;
  approvals answered at `POST /api/session/:id/permission/:requestID/reply`; durable
  cursor-resumable replay at `GET /api/session/:id/event?after=<seq>`; per-session model override at
  `POST /api/session/:id/model`. Driven over localhost HTTP from inside the environment. **No
  TypeScript is required to talk to opencode.**
- **Claude** — the Agent SDK, which spawns the `claude` binary as a subprocess and exposes
  `canUseTool` and lifecycle hooks over its own private channel with that binary. That channel, not
  the subprocess, is why this driver is TypeScript; reimplementing it is precisely the thing this
  project declined to do.

## Interception is a driver capability, never a `Policy` disposition

[`CONTEXT.md`](../../CONTEXT.md)'s four dispositions — allow, deny, judge, human — stay four. If
kestrel ever rewrites a tool's input or narrows a shell command, that is the *driver* narrowing an
operation the policy already allowed: opencode's plugin API on one side, Claude's `PreToolUse` hooks
on the other, and a driver that supports neither simply does not offer the capability. The control
plane never learns the word "plugin," and a runtime that cannot intercept is not broken.

The constraint that follows, and the reason this is recorded now rather than later: **the driver
interface carries a per-tool-call decision point from `0.1`**, unused. That is cheap now and
expensive to retrofit, which is rung one's test.
