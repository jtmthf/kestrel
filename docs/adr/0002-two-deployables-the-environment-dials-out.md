# Two deployables, and the environment dials out

kestrel publishes two images. **`kestrel`** is the control plane: one artifact, roles selected by
argv. **`kestrel-env`** is the base image a run executes in — the supervisor binary, the opencode
binary, the `claude` binary (bundled by the Agent SDK as a platform-specific optional dependency),
and git — with its Dockerfile published so it is derivable rather than opaque. A `Workspace`'s setup
layers project dependencies on top; injecting the supervisor into a bring-your-own image via
`Compute.write_file` stays available as the `0.7` escape hatch.

**The connection between them is opened by the supervisor, outward.**
[#4](https://github.com/jtmthf/kestrel/issues/4)'s compute contract floor is *provision, exec +
stream, read/write files, status, destroy*, and [#14](https://github.com/jtmthf/kestrel/issues/14)
carried it into the `Compute` port unchanged. Dialing *in* would add a seventh capability — a
routable address for a port inside an environment — and that capability splits the eight targets
down the middle, which is the exact reason pause/resume and local disk were already refused. Dialing
out needs only egress, which every substrate has.

**The transport is SSE down, POST up**, over an OpenAPI 3.1 document kestrel publishes and generates
the TypeScript client from — the same pattern kestrel already uses on opencode's spec, one level
down. A boundary you are forced to specify is a better boundary than one you can quietly share types
across. SSE rather than a WebSocket for two reasons: it is plain HTTP, so it traverses every proxy on
all eight targets with no upgrade handshake and sidesteps both unresolved upgrade questions
(Cloudflare's `containerInstance.fetch`, Vercel's `Dockerfile.vercel` containers); and
`Last-Event-ID` *is* the reconnect cursor, given by the protocol rather than invented.

## Roles, and why one image

One control-plane image, role by argv (`serve`, `work`, the CLI), defaulting to every role in one
process. At `0.1` that single process is the **only supported topology** — splitting the roles needs
out-of-process `Fanout` and `Timer`, which would drag Redis or Postgres into rung one.

The one-image rule is not for the on-ramp. The compose path ships a yaml file and every image, so
internal complexity there is free. It exists because Container Apps and Lambda each split into two
deployments at the `Work` seam at `0.7`, which is
[#14](https://github.com/jtmthf/kestrel/issues/14)'s argument for `Work` being the seam at all.

## Consequences

**The supervisor survives a control-plane restart**, which matters because kestrel upgrades itself.
It holds a cursor and nothing else: on reconnect it re-reads opencode from its last acknowledged
sequence, using that server's durable `GET /api/session/:id/event?after=<seq>` replay. The only
events opencode will not replay are the four `.delta` streaming events — which is exactly what
[`CONTEXT.md`](../../CONTEXT.md) already excludes from a `Transcript`, since it records what changed
the session's shared state and never what happened inside a run.
[#14](https://github.com/jtmthf/kestrel/issues/14)'s invariant holds in its strongest form: the
courier carries no cargo.

`kestrel-env` restarts a crashed supervisor, which resumes from its cursor. Supervisor death is not
run death as long as the runtime process lives.

**The definition of done for rung `0.1` includes this test**: `kill -9` the control plane mid-run,
restart it, and the run completes with a gap-free transcript.
