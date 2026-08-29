# `Store` and `Log` are one transactional domain

[#14](https://github.com/jtmthf/kestrel/issues/14) named `Store` and `Log` separately and hedged:
*"It may be a table in `Store`; it is named separately because its access pattern and retention
rules differ."* Resolved: they must be backed by **one transactional system**, and an append commits
together with the state change that caused it or not at all.

**Two settled decisions already depend on it.**
[#19](https://github.com/jtmthf/kestrel/issues/19) made a handoff's brief the successor's first
`Transcript` entry, so enqueueing a run and writing its brief must be atomic — otherwise there is a
window holding queued work whose brief does not exist, on the exact path
[#18](https://github.com/jtmthf/kestrel/issues/18)'s at-most-once dispatch invariant is trying not to
have. And the supervisor's at-least-once replay needs its idempotency check and its append in one
transaction, on the path that runs at every control-plane upgrade.

The alternative was two domains with an outbox and a drainer at rung one, to buy independent
pluggability nothing has asked for.
[#14](https://github.com/jtmthf/kestrel/issues/14)'s own invariant — *a session's entire truth is in
`Store` and `Log`* — is only true if the two cannot disagree, and an outbox makes them able to
disagree for a window.

One transactional system is available on all eight targets (one SQLite file, one Postgres, one
Durable Object's SQLite, DynamoDB transactions) and is free on the compose path, where it is one
`BEGIN`.

## The aggregate boundary

`Session` is the aggregate root, and one transaction touches one aggregate. No exception is carried.

The write that looked like it needed one does not: creating a successor session builds its
`Workspace`, its queued `Run` and its first `Transcript` entry — **one aggregate created** — and
references the enqueuing run by identity, which is what the tactical pattern prescribes for
cross-aggregate references. The enqueuing session is not modified at all, because
[#18](https://github.com/jtmthf/kestrel/issues/18) settled that *enqueueing is causation, not
ownership* and *nothing waits on work it enqueued*.

**Left as fog, deliberately.** At `0.2`, completing a run makes a queued run in another session
eligible, and [#17](https://github.com/jtmthf/kestrel/issues/17) settled that eligibility is a
*write* rather than a computed predicate — two existing aggregates in one transaction. There are no
dependency edges at `0.1`, so no cross-aggregate transaction exists yet and the question gets decided
at `0.2` with code in hand rather than an exception written before the first line.

## Consequence

`Log` is no longer independently pluggable. A future "put the transcript in ClickHouse" is a
read-side projection, not a second `Log` implementation.
