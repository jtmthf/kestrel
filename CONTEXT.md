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
survives restarts. Contains many runs over its life.
_Avoid_: thread, conversation, mission

**Run**:
One execution of an agent runtime inside one environment, on behalf of a session. Has a start, an
end, and an exit status. Work that is queued but not yet started is a run in a queued state.
_Avoid_: job, task, execution, invocation

**Transcript**:
The ordered, replayable record of everything that happened in a session — messages, run boundaries,
participant joins. What a human reads when they join a session late.
_Avoid_: log, event stream, history

### Cause

**Event**:
A single immutable thing that happened in a system kestrel does not own — an issue labelled, a Slack
message posted, a webhook delivered, a schedule elapsed.
_Avoid_: signal, notification, hook

**Trigger**:
A standing, configured rule that matches events and starts work. Named, listable, disableable. A
trigger is the rule, never an individual firing; the session records the event that started it.
_Avoid_: subscription, listener, automation

**Workflow**:
A declared multi-step process whose steps are runs, executing within a single session. Trigger is to
event as workflow is to run: the configured noun declares, the runtime noun happens.
_Avoid_: pipeline, recipe, playbook

### Place

**Environment**:
The isolated compute instance a run executes in. Disposable, provisioned by a compute backend, and
destroyed when finished.
_Avoid_: sandbox, container, machine, box

**Workspace**:
The durable declaration of what a session's work happens against — repositories, branch, setup.
Belongs to the session and outlives every environment built from it.
_Avoid_: checkout, working tree, project

### Actors

**Agent**:
A configured actor identity that participates in a session: its runtime, model, instructions, and
permissions. An agent is configuration, not a running process — a running agent is a run.
_Avoid_: bot, worker, assistant

**Agent Runtime**:
A pluggable implementation of the agent loop that kestrel drives through a contract. opencode is the
default and reference implementation; kestrel does not write an agent loop of its own.
_Avoid_: engine, backend, driver

**Participant**:
A member of a session. A participant is either a human or an agent; the session makes no structural
distinction between them.
_Avoid_: member, user, collaborator

## Invariants

These hold by definition of the terms above; they are stated here because they are what keeps the
words from drifting.

- Every durable record belongs to exactly **one** organization.
- A session has exactly **one** workspace. A workspace may declare **many** repositories.
- A run executes in exactly **one** environment.
- At most **one** run is active in a session at a time.

## Terms deliberately not used

**Job**, **Task**: both name a run, or a state of one. Use **Run**.

**Session** (opencode's): opencode binds one agent conversation to one directory in one process. That
concept lives behind the agent-runtime contract and has no name in kestrel's vocabulary. A kestrel
session contains many of them over its life.

**Agent** (opencode's): opencode uses the word for its own primary/subagent concept. Same resolution
as above — it stays behind the runtime contract.

**Tenant**: names a deployment model, not a thing in the domain. The boundary is an
**Organization**; "multi-tenancy" remains fine as description.

**Team**: a grouping *below* an organization. Deliberately unnamed until something needs it —
an organization is the boundary that is expensive to introduce later, a team is cheap.

**Factory**: describes what kestrel is to a reader; nothing in the system is an instance of it.

**Fleet**: a view, not an entity — every running session looked at once. If the operator surface
needs a name, that is a UI name, not vocabulary.
