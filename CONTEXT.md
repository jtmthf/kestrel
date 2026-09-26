# kestrel

An open-source cloud software factory: background agents, triggered by the events a team already
produces, scheduled onto isolated execution environments, in durable multiplayer workspaces.

This file is the project's glossary. It defines what the words mean — nothing else. Architecture
decisions live in `docs/adr/`.

## Language

### Boundary

**Organization**:
The outermost boundary of ownership and access. Owns projects, triggers, secrets, participants,
and the audit record. A self-hosted install typically has exactly one.
_Avoid_: tenant, account, team, org

**Provider Credential**:
An organization-owned model-provider secret named by the environment variable the harness reads
it from. Held by the organization, never by an agent; encrypted at rest, and reaching an instance
only for the session that spawns an agent with it.
_Avoid_: api key, token, provider key

**Subscription Profile**:
A person's reusable subscription access to a harness, whether an OAuth sign-in or a
subscription-issued key. Owned by that person and used by sessions they authorize, not by an
organization.
_Avoid_: provider credential, shared login

### Work

**Workspace**:
The durable, joinable place a piece of work happens. Owns its history, its participants, its event
log, the checkout and branch resolved from its project, and the instance its sessions execute on,
and survives restarts. Holds many sessions over its life, each with its own agent and its own
context. A workspace is open or sealed; a sealed workspace is readable but accepts no session, no
turn, and no new transcript entry.
_Avoid_: thread, conversation, worktree, mission

**Session**:
One execution of a harness on a workspace's instance: one agent, one context, from start to end.
Has a start, an end, and an exit status, and may contain many turns in one ACP conversation. Work
that is queued but not yet started is a session in a queued state. The model driving the harness's
main loop belongs to the session; what that loop reaches for beneath itself is the harness's
business.
_Avoid_: run, job, task, execution, invocation

**Turn**:
One prompt and response within a session's continuing agent conversation. Its response can be
reported to the work source when the turn finishes, without ending the session.
_Avoid_: session, workspace

**Unfinished Session**:
The one session a workspace may have that has not yet let go of it: queued, mid-turn, between
turns, or ended while its supervisor is still leaving. A workspace has at most one; a message
posted while it exists waits on it rather than starting another.
_Avoid_: slot, current session, holding session

**Active-Work Slot**:
One unit of an organization's capacity for sessions doing work at once. A session mid-turn, or
blocked on an approval, occupies one; a queued session, or one between turns, does not.
_Avoid_: slot (alone), capacity, concurrency

**Unpublished Work**:
Checkout changes or commits that exist only on an instance and cannot be recovered from a remote
repository. Its presence says nothing about whether a session succeeded or the requested work is
good.
_Avoid_: incomplete issue, failed session

**Approval**:
A pending decision that blocks a session until a human resolves it. Always carries a deadline and
is addressed outward through an integration; expiry resolves it as denied. Resolving an approval is
not joining the workspace.
_Avoid_: permission, confirmation, gate, ask

**Question**:
A request for a participant's input that blocks a session until it is answered. Carries a deadline;
on expiry the agent proceeds on its own judgment. A question is about the work, never about
permission — anything that could exceed an agent's policy is an approval, not a question.
_Avoid_: ask, prompt, clarification, input

**Transcript**:
The ordered, replayable record of a workspace, in three kinds. **Shared state**: messages, session
boundaries, participant joins, the branch the workspace works on and the pull request it opened,
and the resolution of every approval and question. **Narration**: what an agent said to itself —
its thoughts and its plans. **Detail**: a session's tool calls and their results. One order and one
cursor across all three; a read names the kinds it wants, and shared state alone is what a human
gets when they join a workspace late. Every entry records a completed unit and never a fragment of
one, so the record has no gaps to reconcile.
_Avoid_: log, event stream, history

### Cause

**Event**:
A single immutable thing that happened, recorded as a CloudEvent: an id, the source that produced
it, the CloudEvents specification version, the type that source calls it, when it occurred, and its
payload. An event from an integration is named in the vocabulary of the system that produced it:
kestrel translates nothing into a vocabulary of its own. kestrel mints events for what kestrel
itself does — a schedule elapsing, and an operator's push — and wraps a POST whose sender named no
event; none of that is translation, and none carries authority for being internal. An Event belongs to one Organization; two Organizations that observe
the same producer event hold separate Events so neither Organization's data is shared with the
other. An Event does not own or select an Integration; reviewed configuration chooses the
connection whose authority kestrel uses.
_Avoid_: signal, notification, hook, payload

**Work Item**:
A unit of work tracked in an external system — an issue, a pull request, a thread — that events
are about and a delegation hands to kestrel. kestrel holds no copy of it; whether it is open,
closed or blocked is read from its integration.
_Avoid_: ticket, issue, task

**Trigger**:
A standing, configured rule that starts work: what it matches, the brief it renders, the agent it
starts that work with, and the project a workspace it opens is resolved from. It declares what a
firing does to an open workspace it correlates to: continue that workspace's waiting session, or
start a new session with the trigger's agent. It may allow other agents that a label on the work
item, or the command that delegated it, can choose instead. Named, listable, disableable, and
bounded by a firing budget. A trigger is the rule, never an individual firing; the workspace
records the event that started it. A trigger matches only events recorded after it was declared:
working the history a repository already holds is a deliberate act, and declaring a trigger is not
it. One that declares a schedule — an interval, or a cron expression read in a declared time zone
— in place of a match fires on its own elapsing. A trigger may name a workflow; when it does, each
firing begins a campaign, and the workspace it opens belongs to that campaign.
_Avoid_: subscription, listener, automation, matcher

**Firing**:
One trigger matching one event, and what that match does: opens a workspace, feeds an open one it
correlates to, records an ignored correlation miss, or holds until its work item is ready. Feeding
continues the workspace's waiting session or starts a new one, as the trigger declares. A held
firing is looked at again and either opens, stays held, or is canceled.
_Avoid_: match, activation, invocation, execution

**Brief**:
The instruction a session starts with, and its entry in the workspace's transcript. A trigger
renders it from a human-authored template over an event and any instruction its dispatch supplied;
an operator may supply it directly. It reaches the agent exactly as rendered.
_Avoid_: prompt, task, instruction, request

**Correlation**:
The key that decides whether a workspace for this work already exists. Rendered from the event by
the trigger, held by the workspace, and unique among an organization's open workspaces.
_Avoid_: dedup key, thread id, subject

**Integration**:
A configured, credentialed connection to an external system. Carries events inbound and kestrel's
requests outbound; an integration may do either direction or both, and declares which. Slack,
Linear, GitHub and a plain webhook are all integrations. It is also the identity kestrel, and the
agents it runs, act under in that system.
_Avoid_: connector, provider, app, plugin

**Delegation**:
A work item deliberately handed to kestrel. The source may express it as an assignment, an agent
delegate, or another explicit act of handing it over.
_Avoid_: assignment, claim, handoff

**Outcome**:
The final record of one session's exit status and what its agent said last, distinct from each
turn's response. Composed when the session ends; an event-started workspace may carry it back
through its integration.
_Avoid_: result, notification, callback, reply

**Workflow**:
A standing, declared process: the roster of agents that may be enqueued, and the caps and failure
tolerances that bound one enactment of it. The sequence is not declared — a session grows it at
runtime by enqueueing further sessions, into its own workspace or into a new one — but nothing
outside the roster may be enqueued. Trigger is to firing as workflow is to campaign: the configured
noun declares, the runtime noun happens.
_Avoid_: pipeline, recipe, playbook

**Campaign**:
One enactment of a workflow: the workspaces and sessions it has enqueued, the concurrency and spend
caps binding them, and the scope a cancellation applies to. A campaign owns no work of its own, and
a workspace belongs to at most one campaign.
_Avoid_: execution, batch, initiative, rollout

### Place

**Control Plane**:
The durable process kestrel runs as. It holds the store, serves the link instances dial out to,
and dispatches sessions onto them. Everything an operator asks for and everything a participant
commands is decided here; an instance is the only thing that executes outside it.
_Avoid_: server, backend, daemon, coordinator

**Role**:
One of two jobs a control plane performs, selected on the command line: `serve` answers the link and
the clients, and `work` claims and dispatches sessions. A role is how a control plane may be split
across processes, never a different program.
_Avoid_: mode, command, service, process

**Client**:
A program a person drives that reaches a control plane over a published boundary, holding no store of
its own and executing nothing. The CLI is the first one.
_Avoid_: frontend, console, cli, tool

**Project**:
The durable declaration of what work happens against — repositories and their defaults, including
a default environment. Declared
once and reused by many workspaces; it is what a workspace's checkout is resolved from, never the
checkout itself.
_Avoid_: repository set, checkout, working tree

**Environment**:
The declaration of what a workspace's instance is provisioned from: its image, its size, and the
setup layered over them. Named, declared once, and derivable from a repository's own
`devcontainer.json`. A workspace's is fixed when it opens: its project's default, unless the trigger
or dispatch that opens it names another. Never the instance itself.
_Avoid_: image, template, class, runner

**Instance**:
The isolated compute a workspace's sessions execute on, provisioned from its environment. Lives
until the workspace seals, and is never reaped while it holds work that exists nowhere else. A
backend may suspend an idle one and resume it unasked; kestrel never learns that it did.
_Avoid_: sandbox, container, machine, box

**Compute Backend**:
A pluggable implementation that provisions and destroys instances, which kestrel drives through a
contract. kestrel tells it when an instance is idle; what it does about that is its own business.
Docker is the default; hosted backends are drivers alongside it.
_Avoid_: provider, infrastructure, cloud, executor

### Actors

**Agent**:
A configured actor identity: its harness, model, instructions, and the policy granted to it. It
takes part in a workspace through the sessions it runs. An agent is configuration, not a running
process — a running agent is a session.
_Avoid_: bot, worker, assistant

**Skill**:
Reusable instructions and supporting files made available to a Harness. Kestrel manages
organization-owned versions selected by Projects and Agents; a repository may supply its own, and
the harness decides whether and when to load one.
_Avoid_: playbook, command, brief

**Harness**:
An agent loop kestrel drives by speaking the Agent Client Protocol to it. opencode is the default;
kestrel writes no agent loop and owns no contract for one. kestrel is not a harness: it governs one
from outside.
_Avoid_: agent runtime, engine, backend, driver

**Participant**:
A member of a workspace. A participant is either a human or an agent; the workspace makes no
structural distinction between them in the transcript or in turn-taking. Reachability is where they
differ: an agent is reached through its harness, a human only through an integration, or not at
all.
_Avoid_: member, user, collaborator

### Governance

**Policy**:
The standing rule set that resolves an attempted operation to one of four dispositions: allow, deny,
judge, or human. Judge refers the operation to a model that decides against stated criteria; human
raises an approval, and the rule that raises it also names who may resolve it. An organization
declares the ceiling, an agent is granted at most that, and effective policy is the intersection.
_Avoid_: permission, rule, guardrail, ask

**Audit Record**:
The organization-scoped, append-only record of every decision kestrel made unattended: what was
decided, the inputs it was decided from, and the verdict. A governed decision is one kind of entry,
carrying additionally the policy that decided it and who resolved it. Distinct from a transcript — a
transcript is one workspace's narrative, an audit record spans every workspace in the organization,
and outlives them.
_Avoid_: log, trail, history, ledger

## Invariants

These hold by definition of the terms above; they are stated here because they are what keeps the
words from drifting.

- Every durable record belongs to exactly **one** organization.
- A provider credential is held by an **organization**, and reaches an instance only for the
  length of a **session**. An idle or destroyed instance holds **none**. An instance outlives the
  sessions it serves, so it holds a credential for a **fraction** of its own life.
- A workspace has exactly **one** project. A project may declare **many** repositories, and many
  workspaces reuse it.
- An event supplies **data**, never **authority**. Brief and correlation are rendered from an
  event; agent, project, environment, model and policy are named in a declaration a human
  reviewed.
- Authority comes from the **authenticated request** that minted an event, never from the event.
  An event kestrel minted for itself is no more trusted than one an integration delivered.
- An integration never takes what its **own identity** said as input or as a command.
- A request that does not **authenticate** never becomes an event. One that authenticated and
  matched no trigger is still recorded.
- **Events expire; a transcript's shared-state entries never do.** An event stream is unbounded
  volume from systems kestrel does not control, and a session's narration and detail are unbounded
  volume from an agent kestrel does not write. Only shared state is bounded by work kestrel chose to
  do, and only shared state carries the never-expires promise.
- A trigger renders its brief **once** per firing. A brief that cannot be rendered **fails** the
  firing and starts nothing.
- A workspace fixes its **project**, **environment** and **instance** for its life, and never an
  agent. A session's agent, and the harness and model it started with, are fixed for the
  **session's** life.
- A firing that feeds an open workspace does what its trigger **declares**: continues the waiting
  session, or starts a **new** session with the trigger's agent.
- A label or a command chooses only among agents a trigger **allows**. A label that chooses **two**
  agents, or a choice the trigger does not allow, starts **nothing**.
- A label, `ready-for-agent` included, never starts work, and neither does an ordinary comment. Work
  starts from a **Delegation** or an operator's **dispatch**.
- An automatic start is **held** while its work item is closed, blocked or of unknown readiness, and
  is looked at again on the next event from its integration and at least every few minutes. Losing
  its **Delegation** cancels a held start; a newer request from the same trigger for
  the same work item supersedes it.
- An authorized command or an operator's dispatch **works ahead** of blockers, and its firing
  records which. Once a workspace is open, a blocker never holds what feeds it.
- A workspace's correlation is unique among an organization's **open** workspaces. A sealed
  workspace holds its correlation against nothing.
- A firing **never** interrupts a session. Events arriving while a session is unfinished are
  pending, and drain into **one** transcript entry when it lets go.
- A session executes on exactly **one** instance, provisioned from exactly **one** environment.
- An instance lives until the workspace it serves **seals**, and is **never** reaped while it holds
  work that exists nowhere else.
- kestrel never learns that a backend **suspended** an instance. An idle hint is information, not a
  capability: an adapter that ignores it is expensive, never degraded.
- A workspace's branch is **declared** by kestrel, never invented by an agent. kestrel runs no git
  command; it learns what happened from what a supervisor reports and what an integration delivers.
- Each completed turn can report its response to the work source. A session's final **Outcome** is
  recorded once and said outward when it adds information beyond those responses; saying it changes
  **nothing** about the session's exit status. Exit status says how the execution went, never
  whether the work was any good.
- A **session** is the only thing kestrel executes outside a control plane. Everything else — the
  store, the link, dispatch — happens inside one. A client runs outside and executes nothing: it
  asks, and the control plane decides.
- At most **one** session is unfinished in a workspace at a time. Concurrency is across workspaces,
  **never** within one.
- A session blocked on an approval still occupies an **active-work** slot. A session waiting between
  turns does not. Neither waits on work it has enqueued.
- A workspace belongs to at most **one** campaign, and nothing outside a workflow's roster may be
  enqueued. Naming a non-member is rejected when the work is **enqueued**, never when it is
  dispatched.
- Only a session whose workspace belongs to a **campaign** may enqueue work. A campaign-less session
  enqueues nothing.
- A queued session is dispatched **at most once**. A lease that expires marks its session failed and
  never re-dispatches it.
- A session may wait between turns with its ACP conversation and instance intact. Waiting holds no
  active-work slot; the next prompt continues that same session. A turn ending does not end a
  session.
- A turn in which the agent produced no message, narration or detail **fails** its session: a
  prompt that never became work is not an answer.
- A session's agent conversation is rooted in the checkout of the **first** repository its
  workspace fixed; the workspace's other repositories sit beside it.
- What a harness writes as diagnostics reaches the **operator**, never the transcript: it is not the
  agent speaking.
- kestrel retries **dispatch**, never **work**. A session that started and failed is retried only by
  a workflow enqueueing a new one.
- Enqueued work records the session that enqueued it. Enqueueing grants that session **no** rights
  over what it enqueued.
- A session whose dependencies are many proceeds only when its declared tolerance is met; **all must
  succeed** unless the workflow says otherwise.
- A queued session whose declared tolerance can **no longer** be met is **unreachable**: terminal,
  operator-visible, and never reported as a failure, because nothing failed.
- An approval may be resolved by someone who is **not** a participant, and resolving it does not
  make them one.
- An approval is resolved by a **human**. No agent resolves one, so no session's agent can approve
  its own approval.
- Effective policy is the **intersection** of the organization's ceiling and the agent's grant.
- A question's expiry is **not** a denial; only an approval's is.
- Every approval resolution appears in **both** the workspace's transcript and the organization's
  audit record.
- A workspace's transcript is readable only by its **participants**. A workspace is a read boundary,
  not only a work boundary.
- Nothing reconstructs a workspace's state from its **transcript**. A transcript is read; state is
  held as current values, **never** derived from history.
- A workspace is **open** or **sealed**. Sealing is not deletion: a sealed workspace is readable and
  is **never** reopened. Work that would have continued it opens a **new** workspace, which records
  the sealed one.
- Every decision kestrel makes **unattended** records the inputs it was decided from and its
  verdict, whether or not a policy was consulted.
- A workspace idle for a **day** seals itself only when it has no **Unpublished Work**. A workspace
  with unpublished work needs attention and keeps its instance; an unfinished session also prevents
  sealing, however old the workspace is.
- kestrel **never** expires a **shared-state** transcript entry. Narration and detail have a
  retention window; an entry of any other kind leaves a transcript only by **deliberate deletion**.
- A deletion inside a transcript is **itself recorded** in that transcript, so what a reader sees is
  gap-free.
- Deleting a workspace removes **nothing** from the audit record.
- A campaign is complete when **every** session it enqueued has ended and every workspace it opened
  is sealed.

## Terms deliberately not used

**Run**, **Job**, **Task**: each names a session, or a state of one. Use **Session**.

**Session** (ACP's): the conversation a harness holds against one directory in one process. A
**Session** holds exactly one and dies with it, so it is a session's identifier at the harness
rather than a concept of its own.

**Agent** (ACP's, and common usage): ACP calls the *program* an agent, and common usage calls a
harness with a model in motion — Claude Code, Cursor — an agent. kestrel's **Agent** is a
configured actor identity; the running thing is a **Session**, driven by a **Harness**.

**Environment** (a harness's): the world an agent acts on through its tools. In kestrel that is a
workspace's **Instance** and checkout; kestrel's **Environment** is the declaration an instance is
provisioned from.

**Tenant**: names a deployment model, not a thing in the domain. The boundary is an
**Organization**; "multi-tenancy" remains fine as description.

**Team**: a grouping *below* an organization. Deliberately unnamed until something needs it —
an organization is the boundary that is expensive to introduce later, a team is cheap.

**Factory**: describes what kestrel is to a reader; nothing in the system is an instance of it.

**Ask**: opencode's name for a permission that must be confirmed, and a bare verb everywhere else.
The pending decision is an **Approval**; the request for input is a **Question**.

**Channel**: Slack's word for a room, and it would be read as one. The configured connection to
Slack, or to any other external system, is an **Integration**.

**Fleet**: a view, not an entity — every running workspace looked at once. If the operator surface
needs a name, that is a UI name, not vocabulary.

**Runner**: GitHub's word for a long-lived shared worker drawn from a pool and registered to a
repository, which is close to the opposite of a per-workspace **Instance**. Using it would import
that expectation. The declaration is an **Environment**; the compute is an **Instance**.

**Environment Class**: two words for one thing, and Ona's compromise for the same problem. kestrel
resolves it by moving **Environment** to the declaration rather than qualifying it.
