# The daemon is reached through a filtered proxy

The compose path never gives the control plane the Docker socket. `/var/run/docker.sock` is
mounted into one container — a socket proxy — which forwards the ten requests the Docker driver
makes and refuses everything else with a `403`. The control plane reaches the daemon over TCP,
by `DOCKER_HOST`, and holds no socket at all.

**This is the default, not a hardening step.** The alternative default is a bind-mounted socket in
the container that runs a coding agent's control plane, which is the whole daemon: any image, any
mount, any container on the host, and no record of what was asked. Nobody but the maintainer has
run kestrel yet, so the shipped default is settled while the cost of settling it is one compose
file.

**The filter is the driver's six operations, written out.** The
[`Compute`](0005-six-ports-at-rung-one-are-named-boundaries.md) contract is provision, exec,
read/write files, status and destroy, and the Docker driver performs each of them by executing
`docker`. That is ten requests:

| Method   | Path                             | What the driver is doing        |
| -------- | -------------------------------- | ------------------------------- |
| `HEAD`   | `/_ping`                         | the client negotiating a version |
| `GET`    | `/_ping`                         | the same, on an older client    |
| `POST`   | `/containers/create`             | provision                       |
| `POST`   | `/containers/{id}/start`         | provision                       |
| `GET`    | `/containers/{id}/logs`          | provision: the Environment's output |
| `POST`   | `/containers/{id}/exec`          | exec, read a file, write a file |
| `POST`   | `/exec/{id}/start`               | the same                        |
| `GET`    | `/exec/{id}/json`                | the same: how the command ended |
| `GET`    | `/containers/{id}/json`          | status                          |
| `DELETE` | `/containers/{id}`               | destroy                         |

Images, volumes, networks, builds, swarm, `/info`, `/events` and every write to a container that
is not `create`, `start` or `exec` are refused. So is `POST /containers/create` carrying a bind
mount, which is the one allowed request that could otherwise hand out the host's filesystem: the
driver mounts nothing into an Environment, so the filter allows a mount from nowhere.

**A refusal is legible on both sides.** The client sees `Error response from daemon: Forbidden`,
and the proxy logs the method, the path and the reason at `WARN`, so what was refused is in
`docker compose logs socket-proxy` rather than inferred from a failure.

**An Environment cannot reach the filter either.** The proxy and the control plane share an
internal network that nothing else joins; an Environment is on the network it dials the link over.
An agent that talks the control plane into provisioning something is bounded by this list, and an
agent reaching for the daemon directly finds nothing listening.

## Consequences

- **The filter bounds the API surface, not the container.** `POST /containers/create` is allowed,
  and a create request may still ask for `Privileged`, host namespaces or added capabilities —
  anything that can create containers can eventually own the host. What the filter buys is that
  the blast radius is the driver's list rather than the daemon's, and that everything outside it
  is refused and recorded. A daemon that is not the operator's own machine is `0.7`'s answer, not
  this one's.
- **The proxy is the one container running as root.** The socket belongs to a group whose id
  differs on every host, and the compose path asks an operator for no values, so the container
  holding the socket holds it as its owner rather than by a group id somebody has to look up.
- **An eleventh request breaks the compose path, loudly.** A driver change that reaches for an
  endpoint outside the list fails against the shipped filter with a refusal that names the path.
  That is the intended cost: the list is a claim about what the driver does, and it is checked
  every time the stack runs.
- **Environments share one network.** They reach the link by the control plane's name on it, and
  at rung `0.1` one Run is active per Session and the compose path runs one work role. Isolating
  an Environment from its neighbours is a network per Run, which is not free and which nothing
  yet needs.
