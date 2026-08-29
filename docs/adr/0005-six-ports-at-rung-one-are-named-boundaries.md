# The six ports at rung `0.1` are named boundaries, not interfaces

[#14](https://github.com/jtmthf/kestrel/issues/14) settled six ports — `Store`, `Log`, `Fanout`,
`Timer`, `Work`, `Compute`. At `0.1`, five of them are **concrete modules whose public API is the
port's vocabulary**: one implementation, no interface type, no config, no registry. Only `Compute`
gets real dispatch.

At `0.1` each port has exactly one implementation, and six interfaces with one implementation each is
precisely the untested abstraction the rule of two exists to reject — a lot of indirection standing
between the project and a working pull request, on the rung [`ROADMAP.md`](../../ROADMAP.md) calls
*"the biggest on the ladder, and it is irreducible."*

But the invariant *a session's entire truth is in `Store` and `Log`* is only enforceable if `Store`
and `Log` are nameable things you can grep for. Write direct SQLite calls at `0.1` and by `0.7` the
truth will have leaked into local disk and process memory in forty places, and the extraction becomes
a rewrite — the same failure [`ROADMAP.md`](../../ROADMAP.md) already refuses for in-memory sessions
(*"it would not be a stub you replace, it would be a different program"*). Names buy the discipline;
interfaces would only buy polymorphism that cannot be tested. Extraction happens at `0.7`, against a
second real implementation, which is what the rule of two actually asks for.

`Compute` is the exception because it is already driven twice at `0.1` in practice: the Docker daemon
is the default, and the local-exec escape hatch is a second driver whether or not it is planned.

## What each one is at `0.1`

| Port | At `0.1` |
|---|---|
| `Store` | one SQLite file |
| `Log` | the same file, same transaction — [ADR-0004](0004-store-and-log-are-one-transactional-domain.md) |
| `Work` | `enqueue` / `claim` / `heartbeat` / `complete` / `fail`, no dependency edges |
| `Timer` | an in-process wheel over a due-time index in `Store`; also carries the GitHub poll |
| `Fanout` | a named no-op — nothing subscribes at `0.1` |
| `Compute` | the Docker daemon over a mounted socket, with a filtered socket proxy as the shipped default before anyone but the maintainer runs it |

`Work` arrives nearly whole because [#17](https://github.com/jtmthf/kestrel/issues/17) found the only
thing it cannot express is *a work item not yet eligible to be claimed* — and at `0.1` everything is
immediately eligible, so the missing half costs nothing to omit. `Fanout` ships as an honest no-op
rather than being skipped, because the greppable boundary is the whole point.

## Consequence: the lease lands at `0.1`, and `ROADMAP.md` is amended

[`ROADMAP.md`](../../ROADMAP.md) listed the lease among rung `0.2`'s contents. Without one, an
environment that dies mid-run leaves its run `active` forever, holding the session's one active-run
slot forever, so the session never seals — which contradicts `0.1`'s own promise that a run
interrupted by a restart ends with an explicit exit status. On a rung whose point is that sessions
are durable from the first commit, a permanently wedged session is the durable record being wrong.

`0.1` needs the **lease**, not the **ledger**, and the two are separable.
[#18](https://github.com/jtmthf/kestrel/issues/18)'s rung assignment is untouched in substance:
dependency edges, the deterministic FIFO ready order and `Campaign` all stay at `0.2` and `0.5`.
