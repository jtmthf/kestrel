# The CLI is a client, not a role

The CLI stops being one of the control plane's roles and becomes a separate program, installed on its
own and pointed at a control plane over an HTTP boundary kestrel publishes. `serve` and `work` remain
roles; the CLI is a **client**.

[#68](https://github.com/jtmthf/kestrel/issues/68) filed the symptom: the shipped path to a command is
`docker compose exec kestrel kestrel <command>`, and documenting it needed an alias the document then
had to apologise for. The symptom is not the reason. The CLI role takes a `Store` and opens SQLite in
process, so it is co-located with the database by construction — it is not a short spelling of a
client, it is the way into the process that holds the file. That works for exactly as long as the
control plane is on your laptop, and `0.7` is the rung where it is not.

## What this amends

[ADR-0002](0002-two-deployables-the-environment-dials-out.md) says kestrel publishes two images and
lists the roles as `serve`, `work` and the CLI. Its argument for one image is entirely about `Work`:
Container Apps and Lambda each split into two deployments at that seam at `0.7`. The CLI appears in
the list and is never argued for, so the one-image rule survives this decision intact — `serve` and
`work` may still be one image, and may later be two for the reason ADR-0002 already gives.

[ADR-0005](0005-six-ports-at-rung-one-are-named-boundaries.md) refuses interfaces with one
implementation, and an operator API has one client today. The cases differ: ADR-0005 rejects
*polymorphism* nothing has yet driven twice, while this is a *process boundary* that has to exist for
the CLI to run anywhere but on top of the database. A boundary with one consumer is still a boundary;
an interface with one implementation is just indirection.

## The boundary

**A second OpenAPI document and a second listener, beside the link and not part of it.** The link is
run-scoped, `runCredential`-bearing, and lives on the network environments dial into. One listener
would mean publishing the link to reach the CLI, handing the Run-credential surface a route it does
not need. Two listeners can be exposed differently because they are for different audiences.

**SSE down, POST up** — the transport ADR-0002 already chose for the link, for reasons that transfer
without modification: plain HTTP with no upgrade handshake, traversing every proxy on all eight
targets, and `Last-Event-ID` as a reconnect cursor given by the protocol rather than invented. The
first consumer exists today, since `session transcript` is already a cursor-paged read.

**The seam must not foreclose streamed output or inbound permission responses.** kestrel's eventual
client is interactive: agent output as it happens, and an approval answered by the person watching.
The agent half of that is already built and already shaped for it —
`kestrel-supervisor`'s `permission` module answers ACP's `session/request_permission`, today with
`allow_once`, and models the subject on ACP v2's union so a human arriving later is not a change to
what kestrel decides about. What is missing is only the path outward, from that decision point to a
client. A request/response API would be designed once now and again at `0.4`; a stream carries new
event types instead.

## Authentication: none, and the listener binds to loopback

There is no operator identity in kestrel. The only principal it has ever had is a Run, holding a
credential issued when its Environment was provisioned and invalidated when it ends. Rather than
invent one here, the operator listener binds to loopback and authenticates nobody, which is precisely
the trust level `docker compose exec` carries today. A remote control plane is reached through a
tunnel until `0.4`, where policy and identity are designed together.

**A bootstrap token was the considered alternative and was rejected for becoming permanent.** A single
key printed beside the database at first boot would authenticate the holder well enough to ship, and
that is the problem: it would still be there at `0.4`, having taught every operator a workflow that
the real design has to take away.

## Consequences

- The control-plane image stops carrying the CLI, and the compose healthcheck stops being
  `kestrel organization list` — it becomes an HTTP probe. One path in, through the published
  boundary, rather than two implementations of every operation that must agree.
- `USAGE.md` installs a CLI rather than opening a shell in a container.
- The CLI is shaped for a remote control plane it cannot yet reach: a URL that defaults to localhost
  is not a temporary flag, it is the shape the boundary is designed against.
