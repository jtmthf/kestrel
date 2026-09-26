# Roadmap

> **We are here: `0.2`, kestrel works the backlog.** `0.1` is closed: an issue labelled
> `ready-for-agent` on this repository is worked with no human action after the labelling, a pull
> request lands, and the outcome round-trips to the issue it came from —
> [`docs/acceptance-0.1.md`](docs/acceptance-0.1.md) is the acceptance run and what it found. The repo
> holds the vocabulary in [`CONTEXT.md`](CONTEXT.md) and the direction in [`README.md`](README.md).
> Nothing schedules more than one run in flight per workspace yet, and a trigger declared today fires
> only for what happens next: a repository's existing label history opens nothing until catching one
> up becomes a deliberate act somebody takes.

Seven rungs from an empty repository to v1, and no dates. Each is a `0.N` release, and each is named
by a class of kestrel's own work rather than by a feature list, because the ladder is walked by
kestrel building kestrel: a rung nobody can dogfood does not belong on it. [`README.md`](README.md)
says what v1 means and what the twelve capabilities are. This document says the order they arrive in,
and how you tell that one has landed.

## How to read the ladder

**A rung closes when its class of work is the default path, not when kestrel has done it once.**
"kestrel opened a PR" and "kestrel opens the PRs" are separated by roughly everything that makes
software real, and only the second one is a test. The consequence is accepted rather than regretted:
rungs are slow and few, which is the right shape for a ladder whose top is a stability lock.

**There is no rung on which a workspace is allowed to be ephemeral.** A workspace's whole truth lives in
storage and its transcript from the first commit, so durability is not a feature added to a workspace
later — it is what a workspace *is*. An in-memory first rung would not be a stub you replace, it would
be a different program. That is why there is no "point of no return" marked further up the ladder:
the point of no return is rung one.

**Depth first, breadth once.** The generic CloudEvents endpoint accepts trigger events from any
producer at `0.1`, so breadth arrives on the inbound path from the first rung. The named surfaces
still wait until `0.6`: each needs its adapter and outbound half, and all seven external surfaces
must round-trip. A schedule has no external recipient, so its outcome is visible in kestrel. This
adapter work is delayed until the durable workspace model has stopped moving underneath it. The
order avoids coupling those adapters to the changing work model; it does not ask a team to wait for
an adapter before kestrel can receive its events.

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
| 0.1 | **kestrel opens its own PRs**          | issues labelled `ready-for-agent` are worked by kestrel, not by you in a terminal | trigger ingestion (GitHub and generic CloudEvents), isolated execution, model choice, persistent workspaces, storage (SQLite) |
| 0.2 | **kestrel works the backlog**          | many issues at once; you stop being the queue                 | scheduling                                                                                   |
| 0.3 | **kestrel's work is joinable mid-flight** | you pick up a running workspace instead of reading a finished one | multiplayer, browser Client, queue and work visibility                                        |
| 0.4 | **kestrel asks before it acts**        | kestrel does work you would not have let it do unsupervised   | governance, operator identity, managed Skills, MCP, Trigger inspection                      |
| 0.5 | **kestrel runs multi-step work**       | classes of work that are a sequence, not a single run         | workflows and Campaign operations                                                             |
| 0.6 | **kestrel meets the team where it works** | integrations return outcomes where work began               | seven external surfaces with outcomes, plus schedule                                          |
| 0.7 | **kestrel runs where you run**         | kestrel develops itself on infrastructure that is not your laptop | pluggable storage, the rule of two, the eight targets                                    |
| —   | **v1**                                 | the lock                                                      | —                                                                                            |

### 0.1 — kestrel opens its own PRs

An inbound CloudEvent reaches the generic endpoint, a trigger matches it, a workspace opens, a run is
scheduled on an Instance, and a pull request lands on this repository. The class of work is issues
labelled `ready-for-agent`: they are worked by kestrel rather than by a person in a terminal, and the
rung closes when that is how they are worked by default. GitHub is the dogfood case, not the
ingestion boundary: anything that can POST a CloudEvent can start work here with no adapter or
integration.

**This is the biggest rung on the ladder, and it is irreducible.** A single triggered pull request
needs the trigger path, the scheduler, the compute contract, the harness boundary, durable
storage, the transcript, and SQLite, all at once. Every split anyone has proposed produces a rung on
which the human is still the scheduler, which is the exact state this project exists to reject.
Everything above this rung is addition; this one is creation.

Some of what lands here is invisible on the day it ships and impossible to add cheaply afterwards:
the `Organization` column on every durable record, the transcript's entry granularity and the fact
that state is held as current values rather than replayed out of history, the `sealed` state on a
workspace, the run-held lease, and the bounded-window-plus-paging transcript read. That last one is
needed here rather than at `0.3`, because a resumed run reads the transcript for context, and the
need arrives before any human has ever joined a workspace. The reason this rung first gave for the read
— compute disposable from the first commit — is history: ADR-0018 gave it up, and an Instance now
lives until its workspace seals
([ADR-0018](docs/adr/0018-an-instance-lives-until-its-workspace-seals.md)). The read keeps its seat for
a resumed run whose box is not there to resume into — one archived once its work was safely pushed,
or lost outright — where a fresh Instance is provisioned and restored from the remote, and the
transcript is what the run reads for context. The lease is here rather than at `0.2` for a
neighbouring reason: without one, an Instance that dies mid-run leaves its run active forever,
holding the workspace's one active-run slot, so the workspace
never seals — and a rung that promises an interrupted run ends with an explicit exit status cannot
ship a workspace that wedges permanently. Also settled here by omission: what happens inside a run is
the run's business, not the workspace's, and gets no promise and no name.

### 0.2 — kestrel works the backlog

Many issues at once, and you stop being the queue. The rung is scheduling: a ledger of queued runs
with dependency edges between them, claiming, at-most-once dispatch, and a deterministic FIFO ready
order. The lease itself landed at `0.1`; what arrives here is the graph its expiry unblocks, and the
rule that expiry fails a run rather than re-dispatching it. Workspaces also start sealing themselves
here, on idle expiry, riding the same timer sweep that reaps leases.

Durability arrives with it, because "works the backlog" is false while a run can take its own output
down with it. An Instance now outlives its runs, and is never reaped while it holds work that exists
nowhere else: the supervisor closes every turn by reporting the checkout's git state, kestrel judges
recovery from what is committed, pushed, uncommitted and untracked — never from an agent's assertion
— and a workspace's next run finds the same Instance and the checkout exactly as the last run left it,
so a follow-up can commit and push what a finished turn left behind. kestrel also declares the branch
its runs work on, which is what makes that restore possible and, at `0.3`, the pull request learnable
([ADR-0018](docs/adr/0018-an-instance-lives-until-its-workspace-seals.md),
[ADR-0019](docs/adr/0019-kestrel-declares-the-branch-and-learns-the-pull-request.md)). A cap on live
Instances at the Organization bounds what keeping the box costs.

Concurrency arrives with it, and it lives **across** workspaces and never within one: at most one run
is active in a workspace, always. That is the project's most surprising design decision, and it is what
lets a backlog be worked in parallel without turn-taking inside a workspace becoming a lock problem.

**This is the weakest boundary on the ladder, and it is flagged rather than defended.** A rung that
turns out easy is a better outcome than a rung quietly hiding four subsystems, so the boundary stays
where it is — but "kestrel worked an issue" and "kestrel worked this week's issues" are different
claims, and the second one is where a helper becomes a factory.

### 0.3 — kestrel's work is joinable mid-flight

You pick up a running workspace instead of reading a finished one. Multiplayer is one uniform promise
designed to the weakest transport kestrel supports, so every deployment gets the same guarantees and
the faster ones are only faster.

The rung is bigger than the read `0.1` built, and this is where the transcript catches up. It is
rewritten into three kinds under one order and one cursor — shared state, narration, and detail — and
a read names the kinds it wants, with shared state alone the page a human gets joining late. Reports
stop being drained at run end and go up as they happen, so a workspace is readable in real time and an
agent stuck thrashing is visible while it thrashes, not after
([ADR-0020](docs/adr/0020-the-transcript-records-what-the-runtime-emits-in-kinds.md)).

Learning the pull request lands here too, as shared state rather than run detail: the branch `0.2`
declared correlates the `pull_request` event the integration already delivers, so a joining human
knows a pull request exists without the tool calls that pushed it
([ADR-0019](docs/adr/0019-kestrel-declares-the-branch-and-learns-the-pull-request.md)). Joining is a
second consumer of the same bounded window and cursor a resuming run already uses. A connection is
never the unit of workspace continuity — reconnecting with a cursor is the normal path rather than a
fallback — and presence is best-effort and never gates anything, because a stale presence entry that
could block an approval would deadlock the workspace it was meant to describe.

**The browser Client is where joining surfaces.** It starts and follows Workspaces, shows the live
transcript, shared state, diffs and read-only live files, and lets a person take a turn. It shows the
declared branch, learned pull request or merge request, and unpublished Instance changes, including
committed but unpushed, uncommitted and untracked work. It shows the requested and effective model,
and reports ACP conversation continuity separately from the durable kestrel Workspace: losing harness
context must not look like successful resume. The queue shows FIFO order, active limits and wait
reasons without inventing an estimated start time.
Feedback is prompt, work state is clear, and the view is accessible and responsive. The same event
stream serves the CLI and browser Client through the operator boundary
([ADR-0015](docs/adr/0015-the-cli-is-a-client-not-a-role.md)). Presence is best-effort, never a
prerequisite for turn-taking or approval.

### 0.4 — kestrel asks before it acts

kestrel does work you would not have let it do unsupervised: policy enforced at the execution layer
rather than by prompt, an approval that reaches a human where they already are, and an audit record
wider than the choices policy consults it on — every decision kestrel made unattended records the
inputs it was decided from and its verdict, whether or not a policy was consulted, and a governed
decision is one kind of entry among them. Deliberate deletion lands here too, along with the tombstone
that keeps a transcript gap-free and the rule that deleting a workspace removes nothing from the audit
record.

**This is the rung on which kestrel becomes usable by someone who is not the maintainer.** Below it,
kestrel acts on your repository with no approval path and no audit record: defensible for the one
person who owns it, indefensible for anyone else. The browser Client already makes live work usable
at `0.3`; `0.4` makes shared operation governable, and `0.6` carries it into the team's other tools.

Approvals are proven here over GitHub and the generic webhook rather than over Slack. Building a chat
adapter against a governance model that this rung is still inventing is the coupling depth-first
exists to avoid, and the generic webhook gives approvals a second surface without a second
integration.

**The Client answers Approvals and Questions interactively** and becomes the place for routine
administration. The agent half already exists — the supervisor answers ACP's permission request,
today by allowing once — so this rung adds the path outward to the person watching. OIDC and SAML
authenticate browser and remote CLI operators, identity-provider groups map to Organization access
grants, and a local administrator path bootstraps a single-machine install. Policy authorizes Workspace
discovery, read and join separately; posworkspace of a link grants none of them. Approvals arriving
through Integrations continue to authorize verified external identifiers without requiring a kestrel
account. This fills the operator identity slot [ADR-0015](docs/adr/0015-the-cli-is-a-client-not-a-role.md)
reserved for `0.4`, while retaining the external identity decision in
[issue 16](https://github.com/jtmthf/kestrel/issues/16).

**Operators can inspect decisions as well as answer them.** A Trigger dry-run evaluates a sample or
recorded Event without starting a Workspace or changing what will fire next. It shows matching,
correlation, rendered Brief and the reviewed Agent, Project, model and Policy that would supply
authority. The historical Event trace includes nonmatches, Firings and ignored evaluations, using
the inputs and verdict recorded at the time rather than today's configuration. The Audit Record is
searchable by time, Workspace or Run, actor, attempted operation, Policy and verdict; entries expose
decision inputs and the Policy snapshot and link to their causes and outcomes. Authorized readers
can page through a stable machine-readable export, with secrets redacted from routine output. An
authorized operator may open an interactive Instance shell under Policy, and its operations are
audited rather than bypassing the control plane.

**Skills and MCP become managed capabilities here.** An Organization catalog versions Skills selected
by Projects and Agents. kestrel stages them in each Harness's filesystem convention; the
harness decides whether to load one or run it as a command. A repository copy wins by name unless
Policy denies it; the effective source is visible, staging never overwrites repository files, and
failure to stage a selected Skill fails the Run visibly. Each Run retains the exact managed versions
delivered; agent-advertised use is recorded as a claim, with no inference when the agent reports
none. Kestrel's own MCP tools start with
pending Events ([issue 90](https://github.com/jtmthf/kestrel/issues/90)), and external MCP servers
can be selected from an Organization catalog by Projects and Agents. Stdio is the baseline; HTTP
requires harness capability advertisement, SSE remains a compatibility path, and unsupported
transport fails visibly. Kestrel mediates external tool calls through Policy and the Audit Record,
supplying per-server, per-Run credentials without ambient harness secrets. Event data and unreviewed
repository MCP configuration cannot select tool authority.

### 0.5 — kestrel runs multi-step work

Classes of work that are a sequence rather than a single run. A workflow declares a roster of agents
that may be enqueued plus the caps and tolerances that bound one enactment of it; each firing of a
trigger that names one begins a campaign, and the sequence is grown at runtime by runs enqueueing
further workspaces. Campaigns carry a concurrency cap, a spend cap, and the scope a cancellation
applies to — and cancelling terminates active runs rather than draining, because a cap that stops
only queued work does not bind.

A handoff is an enqueue and never a message. There is no coordination bus: what kestrel delivers is
ordering and once-only dispatch, the brief passes as the new workspace's first transcript entry — the
same shape a trigger already has with an event — and artifacts pass through the workspace on the
branch kestrel declared for the workspace, a name known before any run starts and a source of facts
kestrel receives from the supervisor and the integration rather than inventing for itself, since it
runs no git command ([ADR-0019](docs/adr/0019-kestrel-declares-the-branch-and-learns-the-pull-request.md)).
`0.1` already writes those first entries; this rung only adds a second kind of writer.

**Governance precedes workflows as a constraint, not a preference.** A workflow's approval step is
governance machinery, so `0.4` has to land first. The README lists the two as independent
capabilities; the ladder cannot.

The Client makes a Campaign inspectable as a graph of child Workspaces and dependency edges, with
status, blocked or unreachable reasons, spend and navigation into each Workspace. Authorized people
can post follow-ups, pause and resume a Campaign, or cancel it. Pause stops new dispatch while active
Runs finish; cancel terminates active Runs. There is no separate skip or rewire operation.

### 0.6 — kestrel meets the team where it works

GitHub, Slack, Linear, Jira, Microsoft Teams, GitLab and the generic webhook round-trip, so an
external surface that started a Workspace receives its outcome there. GitLab is a second source-code
host: its repository and issue Events can start work, and its merge request and result are correlated
and reported there. Jira work items and Teams conversations can start work and receive results in
their own context. Each Integration declares its inbound and outbound capabilities; they share the
Event, Trigger, Workspace and Outcome model without pretending to have identical native operations.
Inbound CloudEvents have worked since `0.1`; this rung adds the named adapters and outbound paths.

Scheduled Triggers join the same Event and Firing path, with intervals and time-zone-aware calendar
recurrence, including the [cron acceptance slice](https://github.com/jtmthf/kestrel/issues/198).
A schedule has no external surface to reply to, so its outcome is visible in the Client and CLI.
Bitbucket and Azure DevOps remain possible additions through the generic Integration seam, rather
than named v1 guarantees.

### 0.7 — kestrel runs where you run

kestrel develops itself on infrastructure that is not your laptop. Postgres joins SQLite, and every
pluggable layer ships its second real implementation — the rule of two — which is the same work as
standing the eight deployment targets up, since both are the question of whether a contract survives
being driven twice. Multi-tenancy becomes a capability here, on the `Organization` boundary that has
been in every record since rung one. The Environment arrives as the declaration `0.1` was always
pointing at: an image, a size, and the setup layered over them, named once and selected per run,
which retires the per-language images that carried the interim, with the archive timeout configured
on it ([ADR-0017](docs/adr/0017-the-environment-is-declared-the-instance-is-provisioned.md)). The
advisory idle hint arrives with it: kestrel tells a compute backend when an Instance is idle and lets
the backend's economics do the rest — a hint that is information rather than a capability, because a
backend that ignores it is expensive, never degraded
([ADR-0018](docs/adr/0018-an-instance-lives-until-its-workspace-seals.md)).

The eight targets are the top of the ladder and not the on-ramp. At kestrel's capability floor you
are committing to a server on every one of them — you are renting it monthly instead of running it —
so the free path is docker-compose on your own machine, and it is a first-class one rather than a
demo mode.

## v1 — the lock

v1 is not a separate implementation rung. It is the stability lock applied after the `0.7` product
floor is complete: the project commits to no breaking changes until v2, with semver on its public
API, workspace-preserving migrations, a documented upgrade path and a deprecation policy. Kestrel's
ACP client has been proven against two agents of different lineages. The browser Client handles
routine work and administration after initial installation, while the CLI remains available for
scripting and power use. The twelve capabilities in the README are the content of the freeze; the
lock is the day this project is willing to stop changing its mind. The `0.x` line carries real,
recommended releases, and is where people will live for a long time.

By `0.7`, kestrel should be the thing that moves the marker at the top of this file.
