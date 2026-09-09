# An Instance lives until its Session seals

An Instance is not destroyed when its run ends. It lives until the session it serves seals, and
kestrel never reaps one holding work that exists nowhere else. This reverses the disposability
[ADR-0002](0002-two-deployables-the-environment-dials-out.md) built in at rung one, and it is a
deliberate trade of spend against a failure that has already happened.

On 2026-09-09 a run implemented an issue, ran fmt, clippy and the suite, reported `succeeded`, and
had its environment reaped with the only copy of the work inside it. The issue carries a public
"run succeeded" comment describing code that existed nowhere by the time it was posted. Nothing in
kestrel could have caught it: the exit status comes from the runtime finishing its turn, and a
kestrel that reasons about no git cannot tell work from the *description* of work.

## Why not fix this with restore instead

The first design here kept environments disposable and made their state serializable, on the
reasoning that agent runtimes already write resumable state to disk. The research does not support
it. Across every documented platform, live-state restore is rare and lossy: the only true pause and
resume of filesystem, memory and running processes is E2B's microVM, which is a substrate capability
of exactly the kind [ADR-0002](0002-two-deployables-the-environment-dials-out.md) refused for
splitting the eight targets. Claude Code restores conversation history and explicitly drops in-flight
subagents and shells; Cursor's cache is disk-only and loses running processes and shell exports. No
platform documents a portable, agent-runtime-owned resume format independent of its own compute.
`docs/research/cloud-agent-environments.md` has the citations.

Conversational state survives a cold start; a process tree does not. Keeping the box is the honest
way to keep both.

## The cost, and the two things that bound it

Spend. On EC2 or anything like it, an instance held for a day is a day's bill, and some backends have
hard limits that will simply be hit. Both are accepted. Two mechanisms bound them, and neither is a
new capability:

**An idle hint.** When a run ends and its session has no queued run behind it, the control plane
tells the adapter that the instance is idle, carrying the handle, the time it went idle, and the
deadline at which it will be archived anyway. Nothing comes back. An adapter may suspend, hibernate
to disk, resize down, or ignore it entirely — Docker and LocalExec ignore it. There is no matching
resume signal: whatever an adapter does, the *next* call against that instance must work, with the
adapter absorbing the wake latency, which is how Fly Machines and E2B already behave. kestrel never
learns suspension happened.

That absence is the whole argument. Delete the hint and nothing about kestrel's semantics changes —
only the bill. The six capabilities are things kestrel's correctness depends on, which is why lacking
one splits the targets; this is a thing kestrel says, and an adapter that ignores it is expensive
rather than degraded. [ADR-0002](0002-two-deployables-the-environment-dials-out.md)'s count survives.

**A cap on live Instances**, at the Organization, separate from and larger than the run concurrency
limit — which no longer bounds what is provisioned, now that instances outlive their runs. A count
rather than a spend figure, because a count is enforceable from what kestrel already knows while
currency needs per-backend pricing kestrel has no source for and would have to ask an adapter for,
which is a seventh capability by another route.

## Consequences

- **The reaping gate is git state.** kestrel establishes that an instance's work exists elsewhere —
  branch pushed, nothing uncommitted, nothing untracked — from what the supervisor reports, never
  from an agent's assertion, which is the one thing a run that hallucinated a push will get wrong.
  See [ADR-0019](0019-kestrel-declares-the-branch-and-learns-the-pull-request.md).
- **The idle hint is gated on the same check.** An instance holding work that exists only inside it
  gets no hint, stays hot, and costs money. Suspension is not destruction, but the research says it
  is lossy in practice nearly everywhere it is documented, and the expensive case is the one that
  should be expensive.
- **Under cap pressure, the longest-idle recoverable Instance is archived early.** Queueing alone
  starves: one busy morning and every slot is held by sessions that finished hours ago. Counting only
  instances with queued or active runs would exclude precisely the idle ones the cap exists for.
- **A failed resume is a stated run failure**, and the session's next run provisions a fresh Instance
  from the Environment declaration and restores from the remote. Silently re-provisioning would lose
  whatever was in the box while reporting success, which is the failure this ADR exists to end.
- **The archive timeout is configured on the Environment**, defaulting from the Organization. Time to
  archive is a cost decision and cost is a property of the box; a large instance and a small one
  should not share a clock because they share an owner.
- **A credential's exposure window narrows relative to the box.** A provider credential still reaches
  an instance only for the length of a run, so an instance now holds one for a fraction of its own
  life. The invariant did not change; the ratio did, and in the safe direction.
- **The transcript stops being the recovery path.** Rebuilding a run's work by paging the transcript
  rescued the 2026-09-09 session and worked well, but it worked because the agent happened to write a
  good summary. It remains what a resumed run reads for context, and is no longer what stands between
  finished work and losing it.
