# Concurrency lives across Sessions, never within one

Many Sessions may have an active Run at the same time, but a Session never has more than one. A Run
grows the backlog by enqueueing further work as new, independent Sessions — never by spawning a
second concurrent Run inside its own — so the one-active-run invariant survives 0.2's scheduling
ledger unchanged ([#18](https://github.com/jtmthf/kestrel/issues/18)).

## Considered options

**Strictly serial Runs**, with the ledger only sequencing and never parallelising, was rejected
outright: without genuine concurrency, kestrel cannot reach v1's promise of working many issues at
once.

**Concurrency within a Session** — letting a Run spawn Runs that execute alongside it, in the same
Session — was rejected. It would overturn the invariant [#11](https://github.com/jtmthf/kestrel/issues/11)
already named as needing an ADR to break, and the survey backing this decision found little support
for it: every handoff-style framework (Swarm, the Agents SDK, AutoGen's Swarm team) is single-active
by construction, and even ledger systems that permit concurrency mostly don't require it (Restate
forbids it per key). The two systems that tried the shape closest to it both back out: Airflow
removed SubDAG in 3.0 for `TaskGroup`, and Anthropic's agent-teams fan-in is experimental, disabled
by default, and documents its own stall — *"An orchestration flow that waits on subagent results can
stall."*

**Concurrency across Sessions** is what shipped: enqueueing is causation, not ownership, the same
shape a Trigger already uses to start a Session from an Event. Nothing waits while enqueued work
runs — no Run holds its slot pending work it enqueued, so no Environment sits idle.

## Consequences

- A fanned-out unit of work is a full Session, not a child that owns no place. Unlike Temporal, Step
  Functions, Prefect and Restate — whose children the research found all own nothing — kestrel's
  fanned-out Sessions keep their own Workspace and their own outward-reaching machinery, so a unit
  can raise its own Approval once `0.4` exists. Other engines' runtimes generally can't: Anthropic's
  states *"No mid-run user input... For sign-off between stages, run each stage as its own
  workflow"* — which is what a Session already is here.
- The Transcript's total order is per-Session only; nothing needed to change about how it's read or
  replayed, because concurrency was never proposed to reach inside one.
- No new storage port: dependency edges and claim eligibility are kestrel's own rows in `Store`
  ([#17](https://github.com/jtmthf/kestrel/issues/17)), and `Work.enqueue` is called only once an
  item becomes eligible.
