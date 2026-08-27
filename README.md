# kestrel

kestrel runs coding agents on your own infrastructure, triggered by the events your team already
produces: Slack threads, Linear tickets, GitHub issues and PRs, webhooks, schedules. Sessions live in
isolated environments, survive restarts, and anyone on the team can join one mid-flight.

The point is to move engineers from *in* the loop to *on* the loop. Agent work that only happens when
someone is sitting in front of a terminal stays a novelty, because the human is still the scheduler
and the bottleneck. kestrel takes the events a team already produces, runs the work on infrastructure
you control, reports back to the surface that started it, and lets a person pick the thread up at any
point without losing what came before.

## Status

Early, and honest about it: there is no running code yet. What the repo holds today is the
vocabulary, in [`CONTEXT.md`](CONTEXT.md), and the full planning trail in the issue tracker, where
every decision below is written down with its reasoning and the objections it survived. This document
is the direction, written first so the implementation has something to be judged against.

## What kestrel is

kestrel ships as a self-hostable platform, installed and operated as one system: you point it at
your repositories and your event sources and it runs agents for you. The framework and SDK seams get
extracted later, once they have proven themselves inside something people actually run. Five ideas
carry the whole product, and they are defined precisely in [`CONTEXT.md`](CONTEXT.md):

- **Trigger**: a standing, configured rule that matches events from systems kestrel does not own and
  starts work. GitHub, Slack, Linear, generic webhook, and schedule are first-party, and every one of
  them round-trips, so the surface that started a session receives the result there.
- **Session**: the durable, joinable thread of work. It owns its history and its participants,
  survives restarts, and contains many runs over its life.
- **Run**: one execution of an agent runtime inside one environment, with a start, an end, and an
  exit status. At most one run is active in a session at a time, which is what makes turn-taking a
  correctness property.
- **Environment**: the isolated compute a run executes in. Disposable, provisioned by a pluggable
  compute backend, and destroyed when the run finishes.
- **Workflow**: a declared multi-step process whose steps are runs, executing within a single
  session, with conditional continuation on exit status and a human-approval step.

Participants in a session are humans or agents, and the session makes no structural distinction
between them: a person joining a running session and an agent taking a turn are the same kind of
thing happening to the same record. That symmetry is deliberate, and it is what lets a workflow hand
a step to a human without the session having to become a different object.

## Who it is for

Platform engineering teams at mid-size organizations are the north star. That is where the gains
compound, because one person's setup becomes everyone's throughput and the governance question is a
live one. Solo developers and small teams are the on-ramp, and the way kestrel gets built in the
first place, so the single-machine docker-compose path is a first class experience and not a demo
mode.

There are two people to serve here and they want different things: boring to operate, delightful to
use. Whoever installs kestrel and whoever works in it are rarely the same person, and pretending
otherwise gives you a product that demos well and cannot be run, or one that runs beautifully and
has no users. When the two conflict, the developer surface wins.

The bar for that surface is set by exemplars rather than adjectives: opencode's TUI, Linear,
Tailscale, Stripe. The common thread is products that have put real thought into how people use them
and try to be seamless, and the review question when something is in doubt is whether those teams
would have shipped it.

Flexibility and "it just works" resolve into one rule: opinionated defaults, swappable underneath.
One command yields a working system, with SQLite, Docker, a default model, and opencode already
chosen for you. Configuration is an escape hatch, so any feature that requires configuration to work
at all is a bug, and that holds after the tenth pluggable layer lands.

## What v1 means

v1 is a stability lock. It freezes the implementation and commits to no breaking changes until v2:
semver on the public API, migrations that never lose sessions, a documented upgrade path, and a
deprecation policy. The day it lands is the day this project is willing to stop changing its mind,
which puts it deliberately far out; the `0.x` line carries real, recommended releases and is where
people will live for a long time.

The gate is an internal one. The agent-runtime contract has to be proven against a second runtime,
because a second working implementation is the only thing that shows the seam absorbs churn; adoption
is the right outcome but the wrong line, since it is not something the project controls. Nine
capabilities are the content of that freeze:

1. **Trigger ingestion**: five sources, all round-trip. Generic webhook is the core and the named
   integrations are adapters over it, so a sixth source is a contribution rather than a fork.
2. **Scheduling**: placement, per-organization concurrency limits, bounded retry with explicit
   failure, and a queue that never rejects. Priority is excluded from v1 on purpose, since fairness
   cannot be tuned without production load the project does not have yet.
3. **Isolated execution**: every run in its own environment, provisioned through a compute contract
   kestrel defines rather than a layer kestrel owns.
4. **Model choice**: any provider the configured runtime supports, selectable per agent, with keys
   held per organization and reaching an environment only when a run needs them. Uniform behavior
   across models is not promised, and neither is model availability across runtimes: once the agent
   runtime is pluggable, "any model" is scoped to whichever runtime you are running.
5. **Persistent sessions**: a session survives everything except deliberate deletion, and an
   environment survives nothing. Process restart, environment teardown, and control-plane upgrade all
   preserve the session and its full transcript, and a run interrupted by a restart ends with an
   explicit exit status.
6. **Pluggable storage**: SQLite for the single-machine path, Postgres for production.
7. **Multiplayer**: one uniform promise, designed to the weakest transport kestrel supports, so every
   deployment gets the same guarantees and the faster ones are only faster.
8. **Workflows**: declared sequence, conditional continuation on exit status, human approval. Parallel
   steps are absent by design, because a workflow runs inside one session and only one run is active
   at a time.
9. **Governance**: an audit record, policy enforced at the execution layer rather than by prompt, and
   a real path for routing an approval to a human in the session.

Underneath all of it sits one pluggability rule: every pluggable layer ships at least two real
implementations at v1, one of them the default. A contract with a single implementation is an
untested abstraction, and a release whose whole purpose is to freeze interfaces cannot afford to
freeze one that has only ever been driven once.

## Where it runs

kestrel has to be cheap to adopt for people who are not willing to commit to running a server, so
serverless is a v1 commitment alongside the obvious targets: docker-compose, Kubernetes, ECS, Cloud
Run, Azure Container Apps, Vercel, AWS Lambda, and Cloudflare. Platform-specific adapters are
accepted as the cost of that, because the realtime seam alone spans four genuinely different
connection models and pretending they intersect produces a contract that lies.

The rule about configuration survives the move: zero required *kestrel* configuration, with vendor
credentials treated as the platform's cost of entry rather than as kestrel config. Every supported
target has a one-command path to a first PR with no kestrel config file, and every adapter names a
default compute pairing so that choice never lands on whoever is adopting it.

## What kestrel is not

- **kestrel does not write an agent loop**: opencode is the default and the reference
  implementation, and the agent-runtime contract is what keeps that boundary honest while leaving
  room for Claude Code, Codex, or anything else to drive a run.
- **Not a hosted SaaS**: there is no plan to run one, and the data model only avoids foreclosing it,
  which is why `Organization` is carried by every durable record from the first migration.
- **Not a CI/CD replacement**: kestrel schedules agent work in response to events and leaves your
  pipelines where they are.
- **Not an IDE or a laptop-attached agent**: it has to be runnable on your own machine, via
  docker-compose or Kubernetes, and that is a separate thing from attaching to your editor.

## Prior art

kestrel is not the first thing in this shape and does not claim to be. Open-Inspect is MIT licensed,
built on opencode, ships multiplayer and multi-source triggers, and is worth reading before you read
anything here; its control plane is bound to Cloudflare Durable Objects, which hand it a per-session
single-threaded actor, colocated SQLite, hibernatable WebSockets, and a per-object alarm, and those
happen to be exactly the four hard parts of durable multiplayer. OpenHands covers much of the same
ground, with governance apparently behind an enterprise tier.

Where a capability is already covered, kestrel's answer is to do it better rather than to cede it or
go hunting for an unoccupied corner. In practice that means a control plane that runs on
infrastructure you choose, cooperative multi-tenancy with a per-organization audit record, and
governance in the open core. Ramp reports that its own Inspect writes more than half of its merged
PRs, and that is the number to beat rather than a reason to pick a smaller problem.

## The project

kestrel is Apache-2.0, in [`LICENSE`](LICENSE). It clears enterprise review as cleanly as MIT while
adding a patent grant and a trademark clause, and it is a license this project would still accept if
commercial pressure arrives, which is the test that matters given how expensive relicensing is later.

kestrel will not be relicensed. Contributions come in under the
[Developer Certificate of Origin](https://developercertificate.org/), signed off with
`git commit -s`, and there is no CLA: a CLA's substantive function is relicensing optionality, which
this project is declining on purpose. The DCO already binds it mechanically, so saying it out loud
costs nothing and turns a technicality into a promise you can read.

Governance is BDFL for now: one maintainer, Jack Moore, with a written path to adding more as
contributors show up. There is no `GOVERNANCE.md`, because a paragraph is the right size for a
project at this stage, and inventing a steering committee for a repo with one contributor would be
worse than saying nothing.
