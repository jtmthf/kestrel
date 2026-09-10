# Store repository traits and enum dispatch are deferred to the Postgres rung

The Store's repository modules gain no trait at `0.1`. Each of the six [#103](https://github.com/jtmthf/kestrel/issues/103)
split up — `Sessions`, `Organizations`, `Workspaces`, `Agents`, `Integrations`, `Triggers` — becomes a
trait at `0.7`, dispatched through an enum the same shape `Compute` already uses, once Postgres makes
the second real implementation exist. `dyn Trait` is rejected now, and the conditions that would
reopen it are written down so `0.7` does not have to re-derive them.

[ADR-0005](0005-six-ports-at-rung-one-are-named-boundaries.md) already rejected trait-based ports with
one implementation each as *"precisely the untested abstraction the rule of two exists to reject."* A
repository trait with only a SQLite adapter behind it is that same shape, not a different one: the
discipline is the greppable boundary the named module gives us, and the polymorphism is untested for
exactly the reason it was for the six ports.

## `0.7` is the dated event, not a vague someday

The [`ROADMAP.md`](../../ROADMAP.md) `0.7` row is literal: *"Postgres joins SQLite, and every
pluggable layer ships its second real implementation — the rule of two."* ADR-0005 already points at
that exact rung — extraction happens *"at `0.7`, against a second real implementation, which is what
the rule of two actually asks for."* The trait should be designed **against** Postgres once it exists,
not ahead of it. A trait written today, informed only by SQLite, bakes in SQLite-shaped assumptions —
what a JSON-in-a-column value can do, how ids surface, how errors come back out of the driver — that
get reshaped anyway once Postgres is real. That is the abstraction work being spent twice.

## The `0.7` plan: one trait per module, an enum between them

```rust
enum Sessions {
    Sqlite(sqlite::Sessions),
    Postgres(postgres::Sessions),
}
```

This is the idiom the one port already driven twice today — `Compute` — uses: the `Provisioned` trait
plus `Driver` enum in `crates/kestrel/src/compute/mod.rs`, where the backend is picked once at
`Store::open` and never swapped at runtime. It is not `dyn Trait`, and the reasons are the same two
that put `Compute` on an enum:

- **Async fns in a trait are not dyn-compatible** without boxing every future — `Pin<Box<dyn Future>>`,
  written by hand or by `async-trait`. Enum dispatch avoids both: native `async fn`, and no per-call
  heap allocation.
- **It is the same problem shape `Compute` already solved.** One seam and a backend chosen once is
  the case a second polymorphism style in the same codebase exists only to confuse. `dyn Trait`'s
  actual superpower — choosing an implementation per call, at runtime — is a case nothing here has.

## Conditions that reopen `dyn Trait`

The enum stays the shape unless one of these is true, and `0.7` treats them as the point of decision
rather than re-listing the trade-offs above:

- A **third** backend arrives, and the cost of adding an enum arm starts to bite.
- The backend must be selected **per request or at runtime** rather than once at `Store::open` — for
  example routing different organizations to different databases.