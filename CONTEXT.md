# kestrel

An open-source cloud software factory: background agents, triggered by the events a team already
produces, scheduled onto isolated execution environments, in durable multiplayer sessions.

This file is the project's glossary. It defines what the words mean — nothing else. Architecture
decisions live in `docs/adr/`.

## Language

### Boundary

**Organization**:
The outermost boundary of ownership and access. Owns workspaces, triggers, secrets, participants,
and the audit record. A self-hosted install typically has exactly one.
_Avoid_: tenant, account, team, org

### Work

**Session**:
The durable, joinable thread of work. Owns its history, its participants, and its event log, and
survives restarts. Contains many runs over its life. A session is open or sealed; a sealed session
is readable but accepts no run, no turn, and no new transcript entry.
_Avoid_: thread, conversation, mission

**Run**:
One execution of an agent runtime inside one environment, on behalf of a session. Has a start, an
end, and an exit status. Work that is queued but not yet started is a run in a queued state.
_Avoid_: job, task, execution, invocation

**Approval**:
A pending decision that blocks a run until a human resolves it. Always carries a deadline and is
addressed outward through an integration; expiry resolves it as denied. Resolving an approval is
not joining the session.
_Avoid_: permission, confirmation, gate, ask

**Question**:
A request for a participant's input that blocks a run until it is answered. Carries a deadline; on
expiry the agent proceeds on its own judgment. A question is about the work, never about
permission — anything that could exceed an agent's policy is an approval, not a question.
_Avoid_: ask, prompt, clarification, input

**Transcript**:
The ordered, replayable record of what happened between a session's participants — messages, run
boundaries, participant joins, and the resolution of every approval and question. It records what
changed the session's shared state, never what happened inside a run: an agent's reasoning and tool
calls are the run's business, not the session's. What a human reads when they join a session late.
_Avoid_: log, event stream, history

### Cause

**Event**:
A single immutable thing that happened in a system kestrel does not own — an issue labelled, a Slack
message posted, a webhook delivered, a schedule elapsed.
_Avoid_: signal, notification, hook

**Trigger**:
A standing, configured rule that matches events and starts work. Named, listable, disableable. A
trigger is the rule, never an individual firing; the session records the event that started it. A
trigger may name a workflow; when it does, each firing begins a campaign, and the session it starts
belongs to that campaign.
_Avoid_: subscription, listener, automation

**Integration**:
A configured, credentialed connection to an external system. Carries events inbound and kestrel's
requests outbound; an integration may do either direction or both, and declares which. Slack,
Linear, GitHub and a plain webhook are all integrations.
_Avoid_: connector, provider, app, plugin

**Workflow**:
A standing, declared process: the roster of agents that may be enqueued, and the caps and failure
tolerances that bound one enactment of it. The sequence is not declared — a run grows it at runtime
by enqueueing further sessions — but nothing outside the roster may be enqueued. Trigger is to event
as workflow is to campaign: the configured noun declares, the runtime noun happens.
_Avoid_: pipeline, recipe, playbook

**Campaign**:
One enactment of a workflow: the sessions it has enqueued, the concurrency and spend caps binding
them, and the scope a cancellation applies to. A campaign owns no work of its own, and a session
belongs to at most one campaign.
_Avoid_: execution, batch, initiative, rollout

### Place

**Environment**:
The isolated compute instance a run executes in. Disposable, provisioned by a compute backend, and
destroyed when finished.
_Avoid_: sandbox, container, machine, box

**Compute Backend**:
A pluggable implementation that provisions and destroys environments, which kestrel drives through a
contract. Docker is the default; hosted backends are drivers alongside it.
_Avoid_: provider, infrastructure, cloud, executor

**Workspace**:
The durable declaration of what a session's work happens against — repositories, branch, setup.
Belongs to the session and outlives every environment built from it.
_Avoid_: checkout, working tree, project

### Actors

**Agent**:
A configured actor identity that participates in a session: its runtime, model, instructions, and
the policy granted to it. An agent is configuration, not a running process — a running agent is a
run.
_Avoid_: bot, worker, assistant

**Agent Runtime**:
An agent loop kestrel drives by speaking the Agent Client Protocol to it. opencode is the default;
kestrel writes no agent loop and owns no contract for one.
_Avoid_: engine, backend, driver

**Participant**:
A member of a session. A participant is either a human or an agent; the session makes no structural
distinction between them in the transcript or in turn-taking. Reachability is where they differ: an
agent is reached through its runtime, a human only through an integration, or not at all.
_Avoid_: member, user, collaborator

### Governance

**Policy**:
The standing rule set that resolves an attempted operation to one of four dispositions: allow, deny,
judge, or human. Judge refers the operation to a model that decides against stated criteria; human
raises an approval, and the rule that raises it also names who may resolve it. An organization
declares the ceiling, an agent is granted at most that, and effective policy is the intersection.
_Avoid_: permission, rule, guardrail, ask

**Audit Record**:
The organization-scoped, append-only record of every governed decision: what was attempted, the
policy that decided it, who resolved it, and the outcome. Distinct from a transcript — a transcript
is one session's narrative, an audit record spans every session in the organization, and outlives
them.
_Avoid_: log, trail, history, ledger

## Invariants

These hold by definition of the terms above; they are stated here because they are what keeps the
words from drifting.

- Every durable record belongs to exactly **one** organization.
- A session has exactly **one** workspace. A workspace may declare **many** repositories.
- A run executes in exactly **one** environment.
- At most **one** run is active in a session at a time. Concurrency is across sessions, **never**
  within one.
- A run blocked on an approval still occupies that **one** active-run slot. Nothing else holds that
  slot: a run never waits on work it has enqueued.
- A session belongs to at most **one** campaign, and nothing outside a workflow's roster may be
  enqueued. Naming a non-member is rejected when the work is **enqueued**, never when it is
  dispatched.
- Only a run whose session belongs to a **campaign** may enqueue work. A campaign-less session
  runs and ends; it never grows.
- A queued run is dispatched **at most once**. A lease that expires marks its run failed and never
  re-dispatches it.
- kestrel retries **dispatch**, never **work**. A run that started and failed is retried only by a
  workflow enqueueing a new one.
- A session records the run that enqueued it, if any. Enqueueing grants that run **no** rights over
  the session.
- A run whose dependencies are many proceeds only when its declared tolerance is met; **all must
  succeed** unless the workflow says otherwise.
- A queued run whose declared tolerance can **no longer** be met is **unreachable**: terminal,
  operator-visible, and never reported as a failure, because nothing failed.
- An approval may be resolved by someone who is **not** a participant, and resolving it does not
  make them one.
- An approval is resolved by a **human**. No agent resolves one, so no run's agent can approve its
  own approval.
- Effective policy is the **intersection** of the organization's ceiling and the agent's grant.
- A question's expiry is **not** a denial; only an approval's is.
- Every approval resolution appears in **both** the session's transcript and the organization's
  audit record.
- A session's transcript is readable only by its **participants**. A session is a read boundary, not
  only a work boundary.
- Nothing reconstructs a session's state from its **transcript**. A transcript is read; state is held
  as current values, **never** derived from history.
- A session is **open** or **sealed**. Sealing is not deletion: a sealed session is readable and is
  **never** reopened. Work that would have continued it starts a **new** session, which records the
  sealed one.
- kestrel **never** expires a transcript entry. An entry leaves a transcript only by **deliberate
  deletion**.
- A deletion inside a transcript is **itself recorded** in that transcript, so what a reader sees is
  gap-free.
- Deleting a session removes **nothing** from the audit record.
- A campaign is complete when **every** session it enqueued is sealed.

## Terms deliberately not used

**Job**, **Task**: both name a run, or a state of one. Use **Run**.

**Session** (ACP's): the thread an agent runtime holds against one directory in one process. A
**Run** holds exactly one and dies with it, so it is a run's identifier at the runtime rather than a
concept, and it gets no name here. A kestrel session contains many of them over its life.

**Agent** (ACP's): ACP calls the *program* an agent. kestrel's **Agent** is a configured actor
identity, and one of them may be run by several ACP agents over its life.

**Tenant**: names a deployment model, not a thing in the domain. The boundary is an
**Organization**; "multi-tenancy" remains fine as description.

**Team**: a grouping *below* an organization. Deliberately unnamed until something needs it —
an organization is the boundary that is expensive to introduce later, a team is cheap.

**Factory**: describes what kestrel is to a reader; nothing in the system is an instance of it.

**Ask**: opencode's name for a permission that must be confirmed, and a bare verb everywhere else.
The pending decision is an **Approval**; the request for input is a **Question**.

**Channel**: Slack's word for a room, and it would be read as one. The configured connection to
Slack, or to any other external system, is an **Integration**.

**Fleet**: a view, not an entity — every running session looked at once. If the operator surface
needs a name, that is a UI name, not vocabulary.
