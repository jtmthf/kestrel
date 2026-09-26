# The supervisor lives with its Instance

A supervisor starts when its Instance is provisioned or resumed, dials the link with the Instance's
identity, and stays until the Instance is destroyed. A Run is begun over that link rather than by
starting a supervisor. This amends [ADR-0002](0002-two-deployables-the-environment-dials-out.md),
which assumed one supervisor per Run, and is what lets the Client read an Instance's files and
changes live between Runs, when no Run exists to answer.

## Considered options

**Reach in through `exec`.** The compute contract already carries it, and a one-shot supervisor
invocation would answer any read with no change to the link. Rejected: the control plane serving a
browser is not guaranteed to hold the compute handle, a backend's exec may be slow or metered, and
reaching into an Instance is the direction ADR-0002 refused.

**Snapshot the unpublished work at turn close.** Exact between Runs, because nothing changes a
checkout without a Run. Rejected: it persists unbounded diff bytes as a stand-in for a value that is
live one link away.

**A second, read-only process beside a per-Run supervisor.** Doubles the link identities and still
leaves the Run's supervisor a special case.

## Consequences

- **A provider credential still crosses only at the runtime's spawn** and dies with the Run
  ([ADR-0010](0010-a-provider-credential-crosses-the-link-at-the-spawn.md)). An idle Instance holds
  a link credential, never a provider credential.
- **The supervisor closes its link once the idle hint is sent**, and dials again when the Instance is
  resumed. An open connection must never keep a backend from suspending an Instance.
- **Nothing reads a hinted Instance, so nothing wakes one.**
  [ADR-0018](0018-an-instance-lives-until-its-session-seals.md) hints only an Instance holding no
  Unpublished Work, so everything a hinted Instance holds is on a remote, and the Client points there.
- **Reads travel the existing link**: a request with an id down the stream, its answer streamed up as
  a POST. The work summary is pushed when it changes. The control plane caches both in memory only;
  the turn-close report the reaping gate reads stays durable and is never shown as current state.
- **The supervisor reads git with `GIT_OPTIONAL_LOCKS=0`**, or a read mid-turn can take `index.lock`
  from under the agent.
