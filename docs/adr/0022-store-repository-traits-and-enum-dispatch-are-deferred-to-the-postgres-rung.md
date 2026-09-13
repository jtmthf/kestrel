# Store repository traits and enum dispatch are deferred to the Postgres rung

The Store's repository modules gain no trait before `0.7`. Each of the six modules
[#103](https://github.com/jtmthf/kestrel/issues/103) split out — `Sessions`, `Organizations`,
`Workspaces`, `Agents`, `Integrations`, `Triggers` — becomes a trait at `0.7`, dispatched through an
enum the way `Compute`'s `Driver` already chooses between its drivers, once Postgres makes the second
real implementation exist. `dyn Trait` is rejected now, and the conditions that would reopen it are
written down so `0.7` does not have to re-derive them.

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
enum Sessions<'a> {
    Sqlite(sqlite::Sessions<'a>),
    Postgres(postgres::Sessions<'a>),
}
```

The enum keeps the handle's name and its borrow of a transaction's connection, so nothing that calls
the Store changes. The trait both arms implement is named at `0.7`, with its methods, against Postgres.

The database is chosen once, at `Store::open`, and never swapped. `Compute` already has a choice of
that shape: `Driver` in `crates/kestrel/src/compute/mod.rs`, read from configuration once and never
decided where a Run executes. `Compute` then holds what it provisioned as a `Box<dyn Provisioned>`;
the Store follows `Driver` and does not follow that, for two reasons:

- **Async fns in a trait are not dyn-compatible** without boxing every future — `Pin<Box<dyn Future>>`,
  written by hand or by `async-trait`. `Provisioned` is synchronous, so `Compute` never paid that;
  every Store method is `async`. Enum dispatch keeps native `async fn` and no per-call heap allocation.
- **Nothing selects per call.** `dyn Trait`'s actual superpower — choosing an implementation per call,
  at runtime — is a case the Store does not have, so it buys nothing an enum does not already give.

## Conditions that reopen `dyn Trait`

The enum stays the shape unless one of these is true, and `0.7` treats them as the point of decision
rather than re-listing the trade-offs above:

- A **third** database arrives, and the cost of adding an enum arm starts to bite.
- The database must be selected **per request or at runtime** rather than once at `Store::open` — for
  example routing different organizations to different databases.
