# Roadmap

> **We are here: `0.1`, kestrel opens its own PRs.** No rung is closed. The repo holds the
> vocabulary in [`CONTEXT.md`](CONTEXT.md) and the direction in [`README.md`](README.md); the code
> so far is the two binaries and CI, and a Session that is durable from the moment it is opened.
> Nothing triggers, schedules or executes work yet.

Seven rungs from an empty repository to v1, and no dates. Each is a `0.N` release, and each is named
by a class of kestrel's own work rather than by a feature list, because the ladder is walked by
kestrel building kestrel: a rung nobody can dogfood does not belong on it. [`README.md`](README.md)
says what v1 means and what the nine capabilities are. This document says the order they arrive in,
and how you tell that one has landed.

## How to read the ladder

**A rung closes when its class of work is the default path, not when kestrel has done it once.**
"kestrel opened a PR" and "kestrel opens the PRs" are separated by roughly everything that makes
software real, and only the second one is a test. The consequence is accepted rather than regretted:
rungs are slow and few, which is the right shape for a ladder whose top is a stability lock.

**There is no rung on which a session is allowed to be ephemeral.** A session's whole truth lives in
storage and its transcript from the first commit, so durability is not a feature added to a session
later — it is what a session *is*. An in-memory first rung would not be a stub you replace, it would
be a different program. That is why there is no "point of no return" marked further up the ladder:
the point of no return is rung one.

**Depth first, breadth once.** Four of the five trigger sources arrive together at `0.6`, near the
top, because the generic webhook is the core and the named integrations are adapters over it —
breadth is repetition of a solved problem, while depth keeps changing the shape of the durable
record. Building five adapters against a session model that then moves underneath all five is the
coupling this order exists to avoid. The cost is real and taken deliberately: kestrel is GitHub-only
for most of the ladder, which is worse to use than the alternative, and affordable only because the
person using it for that stretch is the person building it.

**`Organization` is in every durable record from the first migration**, while multi-tenancy is a
`0.7` capability. The boundary is ruinous to introduce late and cheap to carry early, so it is added
at rung one and stays invisible until the rung that makes it mean something. Multi-tenancy could not
have been a rung of its own in any case: kestrel developing kestrel is one organization, so there is
no dogfooding test for the second one.

**Moving the marker at the top of this file is part of each rung's definition of done.** There are no
per-rung checkboxes. A version number is the one claim about progress you can verify against the
release page in five seconds, and half-ticked rungs would re-litigate once-versus-default-path on
every read.

## The ladder

|     | Rung                                   | The class of kestrel's own work                              | v1 capabilities                                                                            |
| --- | -------------------------------------- | ------------------------------------------------------------ | ------------------------------------------------------------------------------------------ |
| 0.1 | **kestrel opens its own PRs**          | issues labelled `ready-for-agent` are worked by kestrel, not by you in a terminal | trigger ingestion (GitHub only), isolated execution, model choice, persistent sessions, storage (SQLite) |
| 0.2 | **kestrel works the backlog**          | many issues at once; you stop being the queue                 | scheduling                                                                                   |
| 0.3 | **kestrel's work is joinable mid-flight** | you pick up a running session instead of reading a finished one | multiplayer                                                                                |
| 0.4 | **kestrel asks before it acts**        | kestrel does work you would not have let it do unsupervised   | governance                                                                                   |
| 0.5 | **kestrel runs multi-step work**       | classes of work that are a sequence, not a single run         | workflows                                                                                    |
| 0.6 | **kestrel meets the team where it works** | the surfaces you actually use, all round-tripping           | trigger ingestion (five sources)                                                             |
| 0.7 | **kestrel runs where you run**         | kestrel develops itself on infrastructure that is not your laptop | pluggable storage, the rule of two, the eight targets                                    |
| —   | **v1**                                 | the lock                                                      | —                                                                                            |

### 0.1 — kestrel opens its own PRs

A trigger matches an event, a session opens, a run is scheduled in an environment, and a pull request
lands on this repository. The class of work is issues labelled `ready-for-agent`: they are worked by
kestrel rather than by a person in a terminal, and the rung closes when that is how they are worked
by default.

**This is the biggest rung on the ladder, and it is irreducible.** A single triggered pull request
needs the trigger path, the scheduler, the compute contract, the agent-runtime boundary, durable
storage, the transcript, and SQLite, all at once. Every split anyone has proposed produces a rung on
which the human is still the scheduler, which is the exact state this project exists to reject.
Everything above this rung is addition; this one is creation.

Some of what lands here is invisible on the day it ships and impossible to add cheaply afterwards:
the `Organization` column on every durable record, the transcript's entry granularity and the fact
that state is held as current values rather than replayed out of history, the `sealed` state on a
session, the run-held lease, and the bounded-window-plus-paging transcript read. That last one is
needed here rather than at `0.3`, because environments are disposable from the first commit, so a run
resuming on a cold environment needs the paging read before any human has ever joined a session. The
lease is here rather than at `0.2` for a neighbouring reason: without one, an environment that dies
mid-run leaves its run active forever, holding the session's one active-run slot, so the session
never seals — and a rung that promises an interrupted run ends with an explicit exit status cannot
ship a session that wedges permanently. Also settled here by omission: what happens inside a run is
the run's business, not the session's, and gets no promise and no name.

### 0.2 — kestrel works the backlog

Many issues at once, and you stop being the queue. The rung is scheduling: a ledger of queued runs
with dependency edges between them, claiming, at-most-once dispatch, and a deterministic FIFO ready
order. The lease itself landed at `0.1`; what arrives here is the graph its expiry unblocks, and the
rule that expiry fails a run rather than re-dispatching it. Sessions also start sealing themselves
here, on idle expiry, riding the same timer sweep that reaps leases.

Concurrency arrives with it, and it lives **across** sessions and never within one: at most one run
is active in a session, always. That is the project's most surprising design decision, and it is what
lets a backlog be worked in parallel without turn-taking inside a session becoming a lock problem.

**This is the weakest boundary on the ladder, and it is flagged rather than defended.** If `0.1`
lands with storage and the transcript done properly, `0.2` may turn out small. A rung that turns out
easy is a better outcome than a rung quietly hiding four subsystems, so the boundary stays where it
is — but "kestrel worked an issue" and "kestrel worked this week's issues" are different claims, and
the second one is where a helper becomes a factory.

### 0.3 — kestrel's work is joinable mid-flight

You pick up a running session instead of reading a finished one. Multiplayer is one uniform promise
designed to the weakest transport kestrel supports, so every deployment gets the same guarantees and
the faster ones are only faster.

The rung is smaller than it sounds, because `0.1` already built the read: joining is a second
consumer of the same bounded window and cursor a resuming run already uses. A connection is never the
unit of session continuity — reconnecting with a cursor is the normal path rather than a fallback —
and presence is best-effort and never gates anything, because a stale presence entry that could block
an approval would deadlock the session it was meant to describe.

### 0.4 — kestrel asks before it acts

kestrel does work you would not have let it do unsupervised: policy enforced at the execution layer
rather than by prompt, an approval that reaches a human where they already are, and an audit record
of every governed decision. Deliberate deletion lands here too, along with the tombstone that keeps a
transcript gap-free and the rule that deleting a session removes nothing from the audit record.

**This is the rung on which kestrel becomes usable by someone who is not the maintainer.** Below it,
kestrel acts on your repository with no approval path and no audit record: defensible for the one
person who owns it, indefensible for anyone else. `0.6` is where kestrel becomes *pleasant* for a
second person; `0.4` is where it becomes *permissible*.

Approvals are proven here over GitHub and the generic webhook rather than over Slack. Building a chat
adapter against a governance model that this rung is still inventing is the coupling depth-first
exists to avoid, and the generic webhook gives approvals a second surface without a second
integration.

### 0.5 — kestrel runs multi-step work

Classes of work that are a sequence rather than a single run. A workflow declares a roster of agents
that may be enqueued plus the caps and tolerances that bound one enactment of it; each firing of a
trigger that names one begins a campaign, and the sequence is grown at runtime by runs enqueueing
further sessions. Campaigns carry a concurrency cap, a spend cap, and the scope a cancellation
applies to — and cancelling terminates active runs rather than draining, because a cap that stops
only queued work does not bind.

A handoff is an enqueue and never a message. There is no coordination bus: what kestrel delivers is
ordering and once-only dispatch, the brief passes as the new session's first transcript entry — the
same shape a trigger already has with an event — and artifacts pass through the workspace, whose
branches the enqueuer names, because kestrel does not reason about git. `0.1` already writes those
first entries; this rung only adds a second kind of writer.

**Governance precedes workflows as a constraint, not a preference.** A workflow's approval step is
governance machinery, so `0.4` has to land first. The README lists the two as independent
capabilities; the ladder cannot.

### 0.6 — kestrel meets the team where it works

Slack, Linear, GitHub, generic webhook, and schedule, all round-tripping, so the surface that started
a session receives the result there. This is the rung that pays off the depth-first order, and it is
deliberately repetition rather than invention: the session model it is building against stopped
moving several rungs ago.

### 0.7 — kestrel runs where you run

kestrel develops itself on infrastructure that is not your laptop. Postgres joins SQLite, and every
pluggable layer ships its second real implementation — the rule of two — which is the same work as
standing the eight deployment targets up, since both are the question of whether a contract survives
being driven twice. Multi-tenancy becomes a capability here, on the `Organization` boundary that has
been in every record since rung one.

The eight targets are the top of the ladder and not the on-ramp. At kestrel's capability floor you
are committing to a server on every one of them — you are renting it monthly instead of running it —
so the free path is docker-compose on your own machine, and it is a first-class one rather than a
demo mode.

## v1 — the lock

v1 is not a rung. It is the lock applied the day `0.7` closes: the implementation freezes, the
project commits to no breaking changes until v2, and kestrel's ACP client has been proven against
two agents of different lineages. Putting it on the ladder would re-import the reading that v1 is a feature
set, when what it actually is is the day this project is willing to stop changing its mind. The `0.x`
line carries real, recommended releases, and is where people will live for a long time.

By `0.7`, kestrel should be the thing that moves the marker at the top of this file.
