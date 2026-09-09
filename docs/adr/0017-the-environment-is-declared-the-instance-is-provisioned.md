# The Environment is declared, the Instance is provisioned

`Environment` moves to the declaration — image, size, and the setup layered over them — and
`Instance` becomes the isolated compute a run executes on. An Environment is named, declared once,
and selected per run; an Instance is provisioned from one and belongs to a session.

Every configured noun in kestrel already has a runtime partner: Trigger and Firing, Workflow and
Campaign, Agent and Run. `Environment` was the exception, naming only the running thing, which is
why the operator who needed a Rust toolchain had nowhere to put one and ended up with an uncommitted
Dockerfile in a scratch directory.

## Considered options

**A new word for the declaration, leaving `Environment` on the instance** — `Class`, `Template`,
`Runner` — was rejected. `Runner` is GitHub's name for a long-lived shared worker drawn from a pool,
almost the inverse of a per-session box, and the word would import that expectation. `Class` and
`Template` both survive only as qualifiers: Ona ships "environment class" and says "environment"
whenever it means either, which is the compromise this decision exists to avoid.

**Renaming the instance** is what shipped, and `CONTEXT.md` had already narrowed the field for us.
The Environment entry's `_Avoid_` line read "sandbox, container, machine, box" — four candidates
rejected in writing — and its own definition reached for the fifth unprompted: "the isolated compute
**instance** a run executes in." Promoting the word the glossary was already using descriptively is
the smallest available move, and the pair self-explains: an Instance is an instance *of* an
Environment, so neither word carries the other's meaning.

The rename is expensive — `Environment` appears throughout `USAGE.md`, the invariants and
[ADR-0002](0002-two-deployables-the-environment-dials-out.md) — and it is cheaper now than at any
later rung.

## What an Environment holds

Image or Dockerfile, size, and setup. The field converges on five fields — those three plus secrets
and a network policy — and kestrel takes only three, because the other two already have homes: a
provider credential is held by the **Organization** and reaches an Instance only for the length of a
run, and network egress is
[ADR-0009](0009-the-daemon-is-reached-through-a-filtered-proxy.md)'s filtered proxy. Two places to
look for a credential is exactly what that invariant exists to prevent.

Setup lands here rather than on the `Workspace`, which is where
[ADR-0002](0002-two-deployables-the-environment-dials-out.md) originally assigned it and never
implemented it. Setup is how a box gets a toolchain, and the toolchain is what you select an
Environment *for*; splitting "which image" from "what to install on it" across two nouns means every
real answer spans both.

A repository's own `devcontainer.json` is a **source** an Environment can be derived from, with
kestrel's fields layered on top — Ona's shape, and the right one, because a devcontainer is the
repository's statement about its own toolchain while size and policy are not the repository's
business. It is not a compute backend and not a substitute for the declaration.

## Consequences

- **Selection is per run, defaulting from the Agent.** No surveyed platform documents per-task
  selection by a scheduler, and the absence is explainable rather than cautionary: none of them
  schedule. They run one task in one place on demand, so there is nothing to select against. kestrel
  has a queue, a concurrency limit, dependency edges and a claim, and a scheduler that cannot choose
  where to put work is a strange scheduler whatever else it is.
- **The interim is an ugly committed image, not a wait.** `kestrel-env-rust` ships and is built in
  CI, named in its README as an expedient the Environment declaration will retire. kestrel cannot
  dogfood a Rust backlog from a box with no linker, and "properly later" is how the Dockerfile ended
  up somewhere nobody else could reproduce.
- **`Compute` provisions and destroys Instances**, unchanged in shape and count.
  [ADR-0005](0005-six-ports-at-rung-one-are-named-boundaries.md)'s port survives the rename.
