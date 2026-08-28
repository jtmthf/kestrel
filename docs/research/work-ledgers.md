# Work ledgers, claim/lease protocols, and multi-agent handoff

Resolves [#17](https://github.com/jtmthf/kestrel/issues/17). Primary docs read 2026-08-27. Source read at
`steveyegge/beads@8aaefe2` (2026-08-27), `dbos-inc/dbos-transact-py@5bca975` (2026-08-27),
`crewAIInc/crewAI@main`. GitHub's live API queried against `jtmthf/kestrel` itself.

## The finding: kestrel is one column short of a work ledger, and it is not a lease

[#14](https://github.com/jtmthf/kestrel/issues/14) already specified `Work` as *"durable, leased,
idempotent work items — `enqueue(item)`, `claim(leaseMs) -> item`, `heartbeat`, `complete`, `fail`."*
Every system surveyed here implements exactly that and nothing in the survey argues against it. **What
`Work` cannot express is a work item that is not yet eligible to be claimed.** `enqueue` admits an item to
the ready set at the moment it is written. There is no edge, no predicate, no "not before X closes."

That is the entire difference between a queue and a ledger, and it is the whole of #17's question. Four of
the five ledger systems surveyed keep the dependency structure and the claim in the *same* store and
resolve them in the *same* transaction — and the two that separate them (Temporal, Restate) do it by
running a dedicated server kestrel's eight substrates cannot assume.

The consequence is a fork, and it is the sharpest thing on this page:

- **Option A — `Work` stays a queue.** Blocking edges live in `Store` as kestrel's own rows; a transition
  handler recomputes eligibility when a blocker closes and calls `Work.enqueue` at that moment. `Work`
  never learns that dependencies exist. This preserves the six-port shape exactly.
- **Option B — `Work` grows a ready predicate.** `claim` selects over a graph. This is what beads and
  Airflow do, and both pay for it: beads with a denormalized `is_blocked` column that has a `bd doctor`
  repair for going stale, Airflow with a scheduler loop that takes a write lock on every row of the pool
  table.

**Option A is the answer, and the reason is [#14's](https://github.com/jtmthf/kestrel/issues/14) own
invariant.** A session's truth is in `Store` and `Log`. Dependency structure is truth; ready-ness is a
*derivation* of it. Putting the derivation behind the `Work` port pushes truth into a component the
invariant classifies as a cache. Option A keeps the graph in `Store`, keeps every transition in `Log`, and
leaves `Work` implementable on SQS, a Postgres table, and a Cloudflare Queue without any of them growing a
join.

Three more findings follow, each of which changes something concrete.

**Heartbeats must never touch `Log`.** beads shipped leases as columns on the versioned issues row and had
to migrate them out one release later. The migration says why, in its own words (`0055_move_leases_to_table.up.sql`):

> "`lease_expires_at` / `heartbeat_at` as issues columns meant every claim and every heartbeat was an
> issues-row UPDATE -> a Dolt commit. At fleet scale that coordination chatter was the dominant source of
> unbounded reachable history and of the constant write traffic that starves large catch-up merges."

kestrel's `Log` *is* the Transcript, and a heartbeat is not a transcript event. This is a rule, not a
detail: **liveness state is a mutable `Store` cell; only its transitions (`claimed`, `reclaimed`,
`released`) are appended to `Log`.**

**Only Temporal has a fencing token, and it fences the work item, not the holder.** Temporal's docs:
*"A Task Token is a unique identifier for an Activity Task Execution."* Tokens are unique per execution, so
a completion presented with a token from a superseded attempt is not the current attempt's token. beads has
no epoch at all. DBOS has `application_version` — a *code*-version fence that stops an old build dequeuing
new work — and nothing that fences a resurrected executor. Airflow kills zombies but issues no epoch. This
matters because [#15](https://github.com/jtmthf/kestrel/issues/15) put kestrel's fencing token on the
**run-held lease** — on the *holder*. Temporal's placement is the better one for a ledger: with a
per-item token, two items claimed by the same dead process fence independently, and a reclaim of one does
not invalidate the other.

**The coordination literature never needs a message bus the ledger literature does not already provide.**
Stated plainly because #17 asked for it plainly: of the six coordination systems read, exactly one
(AutoGen Core) has a real pub/sub bus, and its bus is *ephemeral* — it adds routing, not durability. Every
durable handoff surveyed is a write to an addressable durable entity. See [the convergence](#the-convergence-a-handoff-is-a-write-to-a-ledger).

---

## beads (`bd`) — the data model, not the pitch

Read at `steveyegge/beads@8aaefe2`. beads is the closest thing in the survey to what #17 is asking about,
and its history is more instructive than its current state: **every mechanism kestrel would need was added
after the fact, in a numbered migration, because its absence broke something.**

### A node is a row; blocking is an edge table; but "ready" is a denormalized column

The unit is an `issues` row (`0001_create_issues.up.sql`) — 50-odd columns including `status`, `priority`,
`assignee`, `defer_until`, `due_at`, and `work_type VARCHAR(32) DEFAULT 'mutex'`. Edges are a separate
table (`0002_create_dependencies.up.sql`):

```sql
CREATE TABLE IF NOT EXISTS dependencies (
    issue_id VARCHAR(255) NOT NULL,
    depends_on_id VARCHAR(255) NOT NULL,
    type VARCHAR(32) NOT NULL DEFAULT 'blocks',
    ...
    PRIMARY KEY (issue_id, depends_on_id),
```

There are **21 built-in edge types** (`internal/types/types.go`), of which only four are structural —
`blocks`, `parent-child`, `conditional-blocks` (*"B runs only if A fails"*) and `waits-for` (*"Fanout gate:
wait for dynamic children"*). The rest are knowledge-graph edges: `related`, `discovered-from`,
`replies-to`, `duplicates`, `supersedes`, `authored-by`, `approved-by`, `attests`, `caused-by`,
`delegated-from`. A ledger and a knowledge graph in one table.

**"Ready" is not a join.** The ready predicate is
`internal/storage/sqlbuild/ready.go`, and its whole dependency clause is three characters of column:

```go
whereClauses := []string{
    statusClause,                    // default: status IN ('open', 'in_progress')
    "(pinned = 0 OR pinned IS NULL)",
    "is_blocked = 0",
}
```

`is_blocked` is denormalized and recomputed **to a fixpoint** (`RecomputeIsBlockedInTxWithResult`,
`internal/storage/issueops/blocked_state.go`) by a query whose gate logic runs to ~90 lines of correlated
`EXISTS` over two dependency planes. It has a repair command (`bd doctor` → `fix.RecomputeBlocked`, whose
comment says it *"repairs stale `is_blocked` flags"*) and it must be recomputed after every merge
(`RecomputeBlockedAfterMerge`). **A derived ready flag in a replicated ledger is a maintenance surface,
and beads pays for it in three places.** This is the single strongest argument for Option A above.

### Claims are a compare-and-set; leases were bolted on later and are node-local

The claim contract (`issueops/claimer.go`) is honest and narrow:

> "Claim validates and commits the complete request as one atomic compare-and-set mutation: it sets
> Assignee to Actor and Status to StatusInProgress only while the issue's status is built-in StatusOpen or
> a configured active status and the issue is unassigned, assigned to Actor, or assigned to a configured
> claim pool."

and, load-bearing for kestrel:

> "Any actor may claim — the actor is caller-asserted provenance, not authenticated identity, and
> eligibility is decided by the issue's state alone."

`bd ready --claim` (`issueops/readyclaimer.go`) fuses selection and CAS: *"Selection, the compare-and-set
and the hydration share ONE transaction, so the row cannot move between being chosen and being reported."*
It refuses `Limit`/`Offset` outright, because *"a bounded window would report 'nothing to claim' whenever
that window happened to be unclaimable while plenty of other ready work remained."*

Leases arrived in migration **0054**, whose header states the bug in one line:

> "Dead-worker recovery (Gas Station v1.1, wy-5r9j): give a claim a lease. A claim was previously
> permanent — a worker that died mid-task stranded its issue `in_progress` forever."

Lease state now lives in its own table (`0055`): `issue_id` PK, `holder`, `granted_at`, `lease_expires_at`,
`heartbeat_at`, plus `granted_node`. `DefaultLeaseTTL = 5 * time.Minute`. Reclaim is a **CLI command**,
`bd reclaim`, not a daemon — its help says *"Run it from a supervisor on a timer with a window of roughly
2× the claim TTL."*

And the thing kestrel must read twice, from `cmd/bd/reclaim.go`:

> "A lease is only meaningful on the replica that granted it. Every other replica's view of the holder's
> liveness is stale by up to one sync interval, so a reaper elsewhere can revert a unit that is very much
> alive over there."
>
> "Two invariants the guard cannot enforce for you: grace window > sync interval, and lease TTL > sync
> interval."

### Can two agents safely work one ledger? Yes — if they share one database

This is the answer #17 asked for, and it is conditional. **On one database, yes**: the claim is a
transactional CAS and the ready-claim is one transaction. **Across replicas, no**: the leases table is
registered `dolt_ignored` and, in the migration's words, *"Leases are deliberately node-local:
`dolt_ignored` tables do not replicate, which matches what leases already were in reality — only
enforceable on the replica that granted them. Cross-machine claim VISIBILITY still rides status/assignee on
issues."* Cross-machine sync is a **manual `bd dolt push` / `bd dolt pull`** — the docs' own best practice
is *"Sync at session end - Run `bd dolt push` so other agents see your updates."*

There is no fencing token. `row_lock` is the nearest thing, and it is a *conflict-forcing* device rather
than an epoch (`issueops/lease.go`):

> "Dolt has no real row locking and merges concurrent commits cell-by-cell, so two transactions that touch
> DIFFERENT cells of the same issue row (a reclaim writing status, a close writing `closed_at`) merge
> silently instead of conflicting — which would let a reclaim quietly revert an issue the owner just
> closed."

It doubles as the published optimistic-concurrency token (`types.Issue.RowVersion`, exposed on the wire as
`Revision`). That is CAS, not fencing: a reclaimed worker that re-reads and retries wins. The only
protection against a zombie claimant is advisory — `bd heartbeat`'s help: *"Only the current owner may
heartbeat. If the lease has already been reclaimed or the issue closed, heartbeat fails so the worker learns
to stop."* Nothing stops it writing.

### Two beads primitives kestrel should steal by name (not by word)

**`work_type` / `mutex`.** Every bead carries `work_type VARCHAR(32) DEFAULT 'mutex'` —
*"One worker, exclusive assignment (default)"* — against `open_competition`: *"Many submit, buyer picks."*
The exclusivity is a **property of the work item**, declared per row, not a global scheduler mode.

**The merge slot.** From `docs/multi-agent/coordination.md`: *"Serialize conflict-prone work ... with a
merge slot — an exclusive-access primitive only one agent can hold at a time. Each project has one merge
slot bead."* This is kestrel's *"at most one run is active in a session at a time"* invariant, implemented
as a row in the same ledger rather than as a rule in the scheduler. **The mutual-exclusion primitive and
the work item are the same kind of object.** That is a real design option for kestrel's active-run slot and
it costs no new port.

Also worth recording: *"Beads has no agent registry — assignees are plain strings."*

---

## The durable-execution systems

### Temporal — the queue is a service, the claim is a poll, the fence is a token

- **Queue.** *"A Task Queue is a lightweight, dynamically allocated queue that one or more Worker Entities
  poll for Tasks"* ([task queue](https://docs.temporal.io/task-queue)). *"Task Queues with a single
  partition are almost always first-in, first-out, with rare edge case exceptions."* Workers pull:
  *"Workers poll for Tasks in Task Queues via synchronous RPC"*, and *"A Worker Process polls for a message
  only when it has spare capacity."*
- **Claim + lease.** There is no lease column; there are four timeouts
  ([detecting activity failures](https://docs.temporal.io/encyclopedia/detecting-activity-failures)):
  Schedule-To-Start (*"the maximum amount of time that is allowed from when an Activity Task is scheduled to
  when a Worker starts that Activity Task"*), Start-To-Close (*"the maximum time allowed for a single
  Activity Task Execution"*), Schedule-To-Close, and Heartbeat Timeout (*"the maximum time between Activity
  Heartbeats"*). An Activity Heartbeat is *"a ping from the Worker that is executing the Activity to the
  Temporal Service."*
- **Death.** Stated bluntly: *"The Temporal Server doesn't detect failures when a Worker loses communication
  with the Server or crashes. Therefore, the Temporal Server relies on the Start-To-Close Timeout to force
  Activity retries."* **The lease timeout is the only death detector**, exactly as in beads and Airflow.
- **Fencing token.** *"A Task Token is a unique identifier for an Activity Task Execution"*
  ([activity execution](https://docs.temporal.io/activity-execution)) — per-attempt, and the docs warn
  against handing it to a remote service precisely because a retry invalidates it.
- **The log.** Event History is *"an append-only log of Events for your application"*
  ([event](https://docs.temporal.io/workflow-execution/event)) — and **it is capped**: a warning at 10,240
  events, termination past 51,200 events, 2,000 updates, or 10,000 signals, escaped only by
  Continue-As-New. Flagged below against [#10's](https://github.com/jtmthf/kestrel/issues/10) unbounded
  session lifetime.
- **The invariant kestrel already wrote down.** *"Temporal guarantees that there can be at most one
  Workflow Execution with a given ID running at any point in time"*, and *"It is not possible for a new
  Workflow Execution to spawn with the same Workflow Id as another Open Workflow Execution, regardless of
  the Workflow Id Reuse Policy"* ([workflowid-runid](https://docs.temporal.io/workflow-execution/workflowid-runid)).
  Temporal then makes the *collision behaviour* configurable — Fail (*"returns a `Workflow execution already
  started` error"*), Use Existing (*"returns a successful response with the Open Workflow Execution's Run
  Id"*), Terminate Existing. [#10](https://github.com/jtmthf/kestrel/issues/10) already chose a fourth
  (queue, never reject); the point is that the industry's canonical implementation of kestrel's invariant
  treats the collision policy as a *parameter*, and kestrel treats it as a constant.

### DBOS — the entire ledger is one Postgres table, and there is no lease at all

Read at `dbos-inc/dbos-transact-py@5bca975`, `dbos/_schemas/system_database.py`. This is the most directly
transferable system in the survey, because **it is nothing but a `Store`.**

The ledger is `workflow_status`: `workflow_uuid` PK, `status`, `executor_id`, `application_version`,
`queue_name`, `queue_partition_key`, `priority`, `deduplication_id`, `recovery_attempts`,
`workflow_deadline_epoch_ms`, `started_at_epoch_ms`. Step results are `operation_outputs`
(`workflow_uuid` + `function_id` PK). Cross-workflow messaging is `notifications` (`destination_uuid`,
`topic`, `consumed`). There is a `streams` table with `(workflow_uuid, key, offset)` — a per-workflow
append-only log.

**There is no `lease_expires_at` and no `heartbeat_at` anywhere in the schema.** The claim is:

```python
# Lock the fixed candidate set -- never a LIMIT query, whose SKIP LOCKED could slide past a locked head
# and admit out of order. On SQLite this is an unlocked re-read; the RETURNING flip below is the guard.
... .with_for_update(skip_locked=True)
# Start the workflows by marking them PENDING; RETURNING reports exactly the rows this statement flipped
... .values(status=PENDING, executor_id=executor_id, recovery_attempts=recovery_attempts + 1, ...)
   .returning(workflow_status.c.workflow_uuid)
```

(`dbos/_sys_db.py`.) **That comment is the portable claim primitive kestrel needs**, and it is the reason
this file is worth reading rather than the docs: `SELECT ... FOR UPDATE SKIP LOCKED` does not exist in
SQLite, which is [#10's](https://github.com/jtmthf/kestrel/issues/10) default storage. DBOS's answer is to
treat the lock as an *optimisation* and the conditional `UPDATE ... WHERE status = 'ENQUEUED' ... RETURNING`
as the *correctness guard* — trusting only the rows the statement actually flipped. One implementation,
both backends, no dialect branch in the semantics.

**Death of a claimant is not detected.** `dbos/_recovery.py`:

```python
def recover_pending_workflows(dbos, executor_ids=["local"]):
    """Attempt to recover pending workflows for a list of specific executors..."""
```

Recovery is keyed on `executor_id` and is triggered *by that executor at its own startup*, or by an
external supervisor that names the dead executor's id. **A workflow claimed by a process that never comes
back stays `PENDING` forever.** The only bound is `recovery_attempts`, capped at
`DEFAULT_MAX_RECOVERY_ATTEMPTS = 100`, after which the row moves to `MAX_RECOVERY_ATTEMPTS_EXCEEDED` — a
dead-letter state, reached only by processes that *do* come back.

Recovery re-enters through the queue rather than executing directly, and the docstring explains why in a
sentence kestrel should adopt wholesale:

> "Recovery re-enqueues rather than executing directly, so that every recovered workflow starts through the
> queue's atomic ENQUEUED->PENDING dequeue. That handoff admits exactly one runner, which makes duplicate
> recovery requests idempotent."

The nearest thing to a fence is `application_version`: dequeue is gated on
`application_version == app_version` (version-less rows only when the worker runs the latest), so a stale
build cannot pick up work it does not understand. That is a *deployment* fence, not an attempt fence.

### Restate — the server owns the invocation; there is no claim to speak of

Restate inverts the polarity. **Nothing claims work; the server drives the handler.**

- *"Restate tracks every step of your code execution in a **journal**. When you call other services, update
  databases, set timers, or perform any side-effecting operation, Restate records both the operation and
  its result."* On failure, *"Restate replays the journal, skipping completed steps and resuming from
  exactly where it left off."* ([key concepts](https://docs.restate.dev/foundations/key-concepts))
- *"**Stateless services**: The Restate Server stores all state and execution history, delivering them with
  each request."* The service deployment holds nothing. There is no worker identity, so there is nothing to
  lease *to*.
- **Liveness is an inactivity timer, not a heartbeat.** *"The maximum time Restate waits for new journal
  entries from a service before Restate considers it stalled"* — default **1 minute**; then the abort
  timeout (*"Restate will wait for the abort timeout before interrupting the user code"*, default **10
  minutes**); then retry with exponential backoff, `max-attempts = 70`, `on-max-attempts = "pause"`
  ([service configuration](https://docs.restate.dev/services/configuration)). Journal retention after
  completion defaults to 24 hours.
- **The exclusivity primitive is first-class and it is kestrel's invariant verbatim.** From
  [services](https://docs.restate.dev/foundations/services/), a Virtual Object is a *"Stateful entity with
  a unique key"* where *"At most one handler with write access can run at a time per object key. Mimicks a
  queue per object key"*, alongside *"Concurrent execution of shared handlers (read-only)."* A Workflow is
  the same shape with *"The `run` handler executes exactly once per workflow ID"* and *"Shared handlers run
  concurrently with the `run` handler to resolve workflow promises, query state, or wait for workflow
  events."*

**That shared-handler carve-out is the piece kestrel's invariant is missing.** "At most one run is active in
a session at a time" is currently stated as a bare exclusion. Restate shows the useful form: exclusive
*write* handlers serialised per key, plus concurrent *read* handlers that may attach, read the log, resolve
a promise, or answer an approval while the exclusive handler holds the slot. That is exactly how
[#15's](https://github.com/jtmthf/kestrel/issues/15) approval path must behave — a resolution arrives from
Slack while a run holds the slot — and kestrel has no word for it.

Addressing is by URL: *"you need to specify the object key or workflow ID in the URL."* Deduplication is a
header: *"Add an idempotency key to your request header to let Restate deduplicate retries"*, and on a
duplicate *"Restate returns the first invocation's result or lets you attach to it if still running"* —
which is [#10's](https://github.com/jtmthf/kestrel/issues/10) queue-never-reject rule and Temporal's "Use
Existing" arriving a third time.

### Airflow — the pure-SQL dependency ledger, added because it is the one that is only a database

Not named in #17, included because it is the only surveyed system that is *both* a dependency graph and a
claimed queue with nothing but a relational database underneath — i.e. the only existence proof for Option A
at scale.

- **Ready is a scheduler loop, not a query.** The scheduler will *"Examine a batch of DagRuns for
  schedulable TaskInstances or complete DagRuns"* then *"Select schedulable TaskInstances, and whilst
  respecting Pool limits and other concurrency limits, enqueue them for execution"*
  ([scheduler](https://airflow.apache.org/docs/apache-airflow/stable/administration-and-deployment/scheduler.html)).
- **The claim is a row lock on a critical section.** Concurrent schedulers are serialised with
  *"database row-level locks (using `SELECT ... FOR UPDATE`)"*, obtained *"by asking for a row-level write
  lock on every row of the Pool table."* Multiple schedulers *"require `SKIP LOCKED` and `NOWAIT` SQL
  support"* — Postgres 12+ or MySQL 8+, i.e. **the dependency ledger's concurrency story is a function of
  the database dialect.** Another point for DBOS's dialect-free guard.
- **Death is a heartbeat timeout with a name.** Zombie tasks are instances *"running ... despite their
  associated jobs being inactive"*; Airflow *"will find these periodically, clean them up, and mark the
  TaskInstance as failed or retry it if it has available retries"*
  ([tasks](https://airflow.apache.org/docs/apache-airflow/stable/core-concepts/tasks.html)).
- **It already has kestrel's approval state.** The task-instance state list includes `deferred` (*"deferred
  to a trigger"*) and `awaiting_input` — a task waiting for a human response in human-in-the-loop
  workflows. [#15](https://github.com/jtmthf/kestrel/issues/15) reasoned to the same place independently:
  a blocked-on-approval run is *known*-blocked, so the approval deadline governs it and the heartbeat does
  not. Airflow spells that as a distinct state rather than a flag, which is the cheaper representation.

### The ledger side in one table

| | Ready set | Claim | Lease | Heartbeat | Claimant dies | Fencing token |
|---|---|---|---|---|---|---|
| **beads** | `is_blocked = 0` denormalized column + fixpoint recompute | CAS on `status`+`assignee`, fused with selection in one txn | `leases` row, TTL 5 min, **node-local, does not replicate** | `bd heartbeat`, writes only the lease table | `bd reclaim` — a CLI reaper an operator must schedule | **none** (`row_lock` is a CAS revision) |
| **Temporal** | n/a — no dependency model; the workflow is the program | worker long-poll on a Task Queue | Start-To-Close + Heartbeat Timeout | Activity Heartbeat to the service | **timeout only** — server does not detect crashes | **Task Token**, per Activity Task Execution |
| **DBOS** | n/a — enqueue is admission | `FOR UPDATE SKIP LOCKED` + conditional `UPDATE ... RETURNING` | **none** | **none** | **nothing** until that `executor_id` restarts or is externally recovered | `application_version` (deployment fence, not attempt fence) |
| **Restate** | n/a — per-key queue is implicit | **no claim** — the server invokes the handler | inactivity timeout 1 min → abort 10 min | journal entries *are* the liveness signal | server retries (backoff, 70 attempts, then pause) | journal/invocation id; stale attempts lose on replay |
| **Airflow** | scheduler loop over DAG edges + pool slots | `SELECT ... FOR UPDATE (SKIP LOCKED)` on pool rows | job heartbeat threshold | worker job heartbeat | zombie reaper marks failed / retries | **none** |
| **wayfinder / GitHub** | client-side filter over sub-issues + `blocked_by` | `--add-assignee` — **additive, not a CAS** | **none** | **none** | **nothing, forever** | **none** |

---

## The coordination side: what is the addressing model?

#17 asked the right question — not features, addressing. Five distinct models, and only two of them
survive a process restart.

| Framework | How agent A reaches agent B | Where the "next agent" is written | Durable? |
|---|---|---|---|
| **OpenAI Swarm** | agent function **returns an `Agent` object** | Python object reference, in process | No — *"stateless between calls"* |
| **OpenAI Agents SDK** | model emits a **tool call** named `transfer_to_<agent>` | the conversation item list | Only if the caller persists it |
| **AutoGen AgentChat (Swarm team)** | agent emits a `HandoffMessage` naming the target | the team's shared message context | No |
| **AutoGen Core** | **publish to a topic**; subscriptions map topic → agent id | the runtime's subscription table | No — routing only |
| **LangGraph** | graph **edge**, `Command(goto=...)`, or `Send(node, state)` | the graph definition + checkpointed state | Checkpointed, mode-dependent |
| **CrewAI** | position in a list, or a manager LLM's delegation | `Task.context: list[Task]` — object refs | No |
| **A2A** | HTTP POST to the peer's **Agent Card `url`** | the peer's own task store | Yes, on the peer |
| **Temporal + Agents SDK** | handoff executes **inside the Workflow** | Event History | **Yes** |

### The four named by the ticket, in their own words

**OpenAI Swarm.** *"An `Agent` can hand off to another `Agent` by returning it in a `function`"*, and
*"If a function returns an `Agent`, execution will be transferred to that `Agent`."* The addressing model is
a **direct object reference**. It is also explicitly not production software: *"Swarm (experimental,
educational)"*, *"is entirely powered by the Chat Completions API and is hence stateless between calls"*,
*"We recommend migrating to the Agents SDK for all production use cases."*

**OpenAI Agents SDK** (Swarm's supported successor) keeps the semantics and moves the addressing into the
model's vocabulary: *"Handoffs are represented as tools to the LLM. So if there's a handoff to an agent
named `Refund Agent`, the tool would be named `transfer_to_refund_agent`."* On transfer,
*"it's as though the new agent takes over the conversation, and gets to see the entire previous conversation
history"*, adjustable via an input filter. **The address is a tool name and the payload is the whole
history** — i.e. the handoff carries no state of its own, because the log is shared.

**AutoGen** is the only framework in the survey with two genuinely different addressing models in one
product. AgentChat's Swarm team is a baton: *"At each turn, the speaker agent is selected based on the most
recent `HandoffMessage` message in the context"*, *"When an agent generates a `HandoffMessage`, the
receiving agent takes over the task with the same message context"*, and *"all agents share the same message
context."* AutoGen Core underneath is publish/subscribe: *"A topic consists of two components: topic type
and topic source"*, *"A subscription maps topic to agent IDs"*, and *"Direct messaging is one to one: the
sender must provide the recipient's agent ID. On the other hand, broadcast is one to many and the sender
does not provide recipients' agent IDs."* Type-based subscription is preferred because it is
*"portable and data-independent: developers do not need to write application code that depends on specific
agent IDs."*

**LangGraph** addresses by graph position. State is *"A shared data structure that represents the current
snapshot of your application"*; each key has a reducer, and *"If no reducer function is explicitly specified
then it is assumed that all updates to that key should override it."* Control moves by edge —
*"When a Node completes its operation, it sends messages along one or more edges to other node(s)"* — or
dynamically, *"Return `Command` from node functions to update state and route to the next node in a single
step"*, or by fan-out: *"LangGraph supports returning `Send` objects from conditional edges. `Send` takes two
arguments: first is the name of the node, and second is the state to pass to that node"*, for the case where
*"the exact edges are not known ahead of time and/or you may want different versions of State to exist at
the same time."* Durability is a dial with three settings —
[`exit` / `async` / `sync`](https://docs.langchain.com/oss/python/langgraph/durable-execution) — where
`exit` *"persists changes only when graph execution exits"* and therefore *"you cannot recover from system
failures (like process crashes) that occur mid-execution."*

**CrewAI** has the weakest ledger of the five and says so in its type signature. Dependencies are
`context: list[Task] | None` — *"Other tasks that will have their output used as context for this task"*
(`lib/crewai/src/crewai/task.py`) — a list of live Python objects, not ids. *"In CrewAI, the output of one
task is automatically relayed into the next one."* There are exactly two processes
(`lib/crewai/src/crewai/process.py`):

```python
class Process(str, Enum):
    sequential = "sequential"
    hierarchical = "hierarchical"
    # TODO: consensual = 'consensual'
```

In hierarchical mode *"Tasks are not pre-assigned; the manager allocates tasks to agents based on their
capabilities"* — **the assignment decision is a model call**, which is the only genuinely novel addressing
model in the survey and also the only one with no durable record of why an agent got the work.

### Handoff over a durable log, where it exists

**A2A** is the only *protocol* here, and its data model is nearly kestrel's. An agent is addressed by its
Agent Card — *"A JSON metadata document published by an A2A Server, describing its identity, capabilities,
skills, service endpoint, and authentication requirements."* A Task is *"a unique identifier for a Task
object, representing a stateful unit of work with a defined lifecycle"* and is **server-generated**; a
Context Id *"logically groups multiple related Task and Message objects, providing continuity across a
series of interactions."* **That is kestrel's Session/Run split with the names swapped.** The lifecycle —
`submitted`, `working`, `input-required` (*"the agent requires additional user input to proceed"*),
`completed`, `failed`, `canceled`, `rejected` — includes kestrel's Question/Approval blocked state as a
first-class task state, exactly as Airflow's `awaiting_input` does. Retrieval is pull (`GetTask`) with
optional webhook push. **There is no claim and no lease in A2A**: the executing agent owns the task, and the
client has no way to take it back.

**Temporal + the OpenAI Agents SDK** is the one place the two literatures are the same product.
*"the agent loop, tool selection, and handoffs"* run inside the Workflow, while *"model calls are executed
as Activities, so they retry durably and are not repeated during Workflow replay."* The result:
*"Your agents survive Worker restarts and can run for extended periods without losing state."* A handoff is
therefore a **command recorded in the Event History** — an entry in the durable log, replayed on recovery.
LangGraph reaches the same place by a different route (checkpoint per superstep, `durability="sync"`), one
notch weaker: a checkpoint is a *snapshot of state*, not an append-only record of transitions, so it
supports resumption but not the cursor-replay [#15](https://github.com/jtmthf/kestrel/issues/15) requires.

---

## The convergence: a handoff is a write to a ledger

#17 asked whether the coordination literature ever needs a message bus the ledger literature does not
already provide. **It does not, and the survey is close to unanimous.**

Of eight coordination mechanisms read, exactly one — AutoGen Core — has a message bus, and that bus is
**ephemeral**: topics and subscriptions are a routing table, not a store, and nothing in it survives a
process restart. Its own docs sell it on *portability of addressing*, not on delivery guarantees. Every
other mechanism either (a) writes the successor's name into shared state that something else reads
(LangGraph, AutoGen AgentChat, CrewAI), (b) names the successor directly in-process (Swarm), (c) emits a
tool call that the runner interprets (Agents SDK), or (d) POSTs to the peer's own durable task store (A2A).

Read the other direction, the equivalence is exact:

- A **claimed ready-queue over a DAG coordinates N agents with no messaging layer at all.** beads is the
  proof: `bd ready --claim` is the entire coordination protocol, and its docs' "handoff pattern" is literally
  two writes — *"`bd comment bd-42 "API complete, ready for review"` / `bd assign bd-42 agent-b`"*.
- A **handoff is a write to a ledger.** Restate's `send` is an append to a per-key queue. Temporal's handoff
  is an Event History entry. A2A's `SendMessage` creates a Task row on the peer. The difference between
  "messaging" and "ledger writing" here is which side of an HTTP boundary the row lands on.
- The one thing a bus adds that a ledger cannot is **latency**: a poller finds ready work in O(poll
  interval); a notification finds it immediately. That is precisely
  [#14's](https://github.com/jtmthf/kestrel/issues/14) `Fanout` — *"best-effort, at-most-once ... It
  carries no payload that matters."*

**So the ruling for kestrel is: no new port.** A dependency ledger with claims and leases needs `Store`
(rows, CAS), `Log` (transitions), `Fanout` (wake a poller), `Timer` (lease expiry sweeps) and `Work`
(execution of what is already ready). Five of the six ports #14 already named, used for a purpose #14 did
not have in mind, and **nothing in the survey requires a message bus, a broker, or a sixth store.**

One caveat, and it is real: **`Timer` is now load-bearing for correctness, not just for schedules.** Lease
expiry is a due-time sweep. #14 established every substrate's timer is *"at-least-once, no precision
guarantee, no per-entity addressing"*, and Vercel Hobby's is once per day with ±59 minutes of jitter. A
lease TTL cannot be shorter than the tick that reaps it — which is beads' *"lease TTL > sync interval"*
invariant arriving from the substrate side. On the weakest substrate, **kestrel's minimum honest lease TTL
is a day.** That is a finding for the Vercel adapter's already-documented Pro floor, and it is a stronger
argument for that floor than the cron requirement was.

---

## Does the model *require* concurrency, or merely permit it?

#18 turns on this, so it is stated per system rather than in prose.

| System | Requires concurrency? | Notes |
|---|---|---|
| **beads** | **Permits.** | One agent claiming from one ledger is the documented single-agent path. `work_type` defaults to `mutex` — *"One worker, exclusive assignment (default)"* — and the merge slot exists to *remove* concurrency where it hurts. |
| **Temporal** | **Permits.** | *"at most one Workflow Execution with a given ID running at any point in time"* is enforced, not optional. Requires a **worker process separate from the service**, which is a deployment constraint, not concurrency of work. |
| **DBOS** | **Permits.** | Queues are optional entirely; `worker_concurrency` may be 1; DBOS runs inside the application process. The closest thing to a requirement is that *something* must restart to recover a dead executor's rows. |
| **Restate** | **Forbids, per key.** | *"At most one handler with write access can run at a time per object key"* — the model actively serialises. Concurrency exists only across keys and in read-only shared handlers. Requires a separate server process. |
| **Airflow** | **Permits.** | Parallelism is the point but `max_active_tasks = 1` is legal; multiple schedulers are optional and gated on `SKIP LOCKED` support. |
| **A2A** | **Permits.** | A Task is owned by one agent; nothing requires two tasks in flight. Requires two *processes* (client and server) by construction. |
| **OpenAI Swarm / Agents SDK** | **Forbids, effectively.** | The handoff model is a baton: exactly one agent holds the conversation. This is kestrel's invariant with no work needed. |
| **AutoGen AgentChat (Swarm team)** | **Forbids.** | *"the speaker agent is selected based on the most recent `HandoffMessage`"* — strictly one speaker per turn. |
| **AutoGen Core** | **Requires.** | The runtime is a concurrent message-passing system; broadcast is one-to-many by definition. |
| **CrewAI (sequential)** | **Permits.** | `async_execution` is opt-in per task. |
| **LangGraph** | **Requires, for fan-out graphs.** | Multiple outgoing edges from one node execute in the same superstep, and `Send` exists specifically for *"different versions of State to exist at the same time."* A linear graph never needs it; a map-reduce graph cannot avoid it. |

**The finding for #18:** the two families split cleanly, and not the way the framing suggests. **Every
handoff-based coordination framework is single-active-agent by construction** — Swarm, the Agents SDK, and
AutoGen's Swarm team all pass a baton, and every one of them would satisfy kestrel's invariant unmodified.
**Every dependency-ledger system merely permits concurrency**, with Restate actively forbidding it per key.
The only two mechanisms in the survey that *require* concurrency are AutoGen Core's bus and LangGraph's
fan-out — i.e. **exactly the two mechanisms kestrel would need in order to run a Workflow's steps in
parallel**, which [#10](https://github.com/jtmthf/kestrel/issues/10) already ruled is an ADR against the
invariant rather than a feature.

## Does the ready-queue depend on priority to be usable?

[#10](https://github.com/jtmthf/kestrel/issues/10) excluded priority scheduling from v1. **Nothing in the
survey contradicts that, and one system supports it outright.**

| System | Priority | Required? |
|---|---|---|
| **Restate** | **None.** FIFO per object key. | The strongest support for #10. |
| **Temporal** | Priority Key, *"a value within the integer range `[1,5]`"*, and *"If you don't specify a Priority, a Task defaults to a Priority of `3`"* | No — optional, with fairness keys as a separate, also-optional dial |
| **DBOS** | `priority_enabled` defaults to `false`; *"Workflows with the same priority are dequeued in FIFO (first in, first out) order"* | No — off by default |
| **Airflow** | `priority_weight`, but *"task priority will only come into effect when there are more scheduled tasks waiting than the queue slots"* | No — a contention tiebreaker |
| **beads** | `priority INT NOT NULL DEFAULT 2`; the **default** sort policy is a hybrid that buckets by a 48-hour recency cutoff and then sorts by priority | No — `--sort oldest` is `ORDER BY created_at ASC, id ASC` |
| **wayfinder / GitHub** | *"first in map order wins"* | **Yes — and this is the finding.** |

**beads is the one caution and wayfinder is the one contradiction.** beads' *default* ready order
(`sqlbuild.BuildReadyWorkOrder`, `SortPolicyHybrid`) is `CASE WHEN created_at >= now-48h THEN 0 ELSE 1`,
then priority within the recent bucket. Priority is not needed for correctness, but the tool's authors
concluded a bare FIFO front was not the useful default at agent fleet scale. If kestrel's ledger ever
serves more than one agent, expect the same pressure — and expect it to arrive as a *sort*, which is cheap,
rather than as *admission*, which is the thing #10 actually excluded. **Worth writing that distinction down:
#10 excluded priority *scheduling*; it did not exclude a deterministic ready *order*, and a ledger needs
one regardless.**

---

## What wayfinder's own arrangement actually fails at

The prior art is in this repo. The map (#1) is an issue with 18 sub-issues, blocking edges are GitHub's
native issue dependencies, the frontier query is a client-side filter, and the claim is
`gh issue edit <n> --add-assignee @me` (`docs/agents/issue-tracker.md`). It is a dependency ledger with
claims, rented from GitHub. Judged against the six-column table above, here is what it fails at, in
descending order of how much it matters.

**1. The claim is not a compare-and-set — it is an append.** GitHub's REST docs for the endpoint:
*"Adds up to 10 assignees to an issue. Users already assigned to an issue are not replaced."*
([assignees](https://docs.github.com/en/rest/issues/assignees)) There is no conditional request, no
`If-Match`, no revision. Two agents that both read "unassigned" both succeed, and the issue ends up with two
assignees and two agents working it. beads' `ClaimNext` exists precisely to close this window —
*"Selection, the compare-and-set and the hydration share ONE transaction, so the row cannot move between
being chosen and being reported."* **Wayfinder's claim cannot be in the same transaction as its selection
because the selection happens on the client, over three separate API calls.**

**2. There is no lease, so the claim never expires.** Live evidence from this repo: of #1's 18 children,
every closed ticket from #7 onward still carries `jtmthf` as assignee. **Nothing releases a claim.** The
frontier query survives only because closing removes the ticket from the open set — so an agent that claims
a ticket and dies without closing it removes that work from the frontier *permanently*, with no reaper and
no `bd reclaim` equivalent. This is verbatim the bug beads shipped migration 0054 to fix:
*"A claim was previously permanent — a worker that died mid-task stranded its issue in_progress forever."*

**3. The claimant is a GitHub user, and agents are not users.** #17 is assigned to `jtmthf` — a human —
while an agent does the work. GitHub silently drops assignees without push access
(*"Assignees are silently ignored otherwise"*), so an agent identity requires a real account with write
access to the repository. **The claim therefore records the wrong actor**, which is the opposite of what
[#10's](https://github.com/jtmthf/kestrel/issues/10) governance floor wants from an audit record. beads,
notably, does not pretend otherwise: *"the actor is caller-asserted provenance, not authenticated
identity."*

**4. Ready is computed client-side and cannot be transactional with the claim.** The documented frontier
query is: list the map's open children, drop any with `issue_dependencies_summary.blocked_by > 0`, drop any
with an assignee, take the first in map order. That is three round-trips and a client-side join, against a
5,000-requests-per-hour primary limit, 80 content-generating requests per minute and 500 per hour, and no
more than 100 concurrent requests
([rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)). Every
system in the ledger table resolves selection and claim in one transaction; wayfinder resolves them in four
HTTP calls.

**5. The ordering is priority scheduling wearing a hat.** "First in map order wins" is a hand-maintained
total order over the frontier — which is exactly the thing
[#10](https://github.com/jtmthf/kestrel/issues/10) excluded from v1, reintroduced as the tie-break for the
project's own coordination. It works because a human maintains it. It does not survive N agents.

**6. Structural caps.** GitHub allows *"up to 100 sub-issues per parent issue"* and *"up to eight levels of
nested sub-issues"*
([sub-issues](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/adding-sub-issues)).
The map is at 18. Fine for wayfinding; not a ledger for a fleet.

**What it gets right, and kestrel should keep:** blocking edges are stored *by the tracker*, not in prose,
and the gate is queryable — `issue_dependencies_summary.blocked_by` counts only open blockers, verified
live against `repos/jtmthf/kestrel/issues/18/dependencies/blocked_by`. The graph is durable, visible to
humans in a UI, and survives every agent that touches it. That is the half of the arrangement that works,
and it is the half kestrel would have to build.

---

## Vocabulary: where kestrel needs a different noun

`CONTEXT.md` bans **task**, **job** and **channel**. The survey is built almost entirely out of those three
words.

| Prior art's word | Used by | kestrel's problem |
|---|---|---|
| **Task** | Temporal (Task Queue, Activity Task, Workflow Task, Task Token), Airflow (TaskInstance), A2A (Task), CrewAI (Task), beads (`issue_type` default `'task'`) | Banned outright. **Temporal's "Task Token" has no kestrel spelling at all**, and it is the one mechanism this research says kestrel should adopt. |
| **Job** | Airflow (job heartbeat), Kubernetes | Banned; `Run` covers it |
| **Channel** | **LangGraph — state keys are literally "channels"** | Banned as Slack's word. Any LangGraph-shaped state model imported into kestrel arrives pre-collided. |
| **Step** | DBOS (step), Restate (durable step), Workflow steps | **Already taken, and taken differently.** `CONTEXT.md`: *"Workflow: A declared multi-step process whose steps are runs."* A kestrel step **is** a Run. So a unit of work *below* a Run cannot be called a step. |
| **Lease**, **claim**, **heartbeat**, **reclaim** | beads, Temporal, Airflow | Free, and [#15](https://github.com/jtmthf/kestrel/issues/15) already uses *lease* and *fencing token*. Keep them. |
| **Ledger** | this survey | **Already listed as an avoid-word** — `CONTEXT.md`'s **Audit Record** says *"Avoid: log, trail, history, ledger."* Whatever this thing is called, it is not a Ledger. |
| **Handoff** | Swarm, Agents SDK, AutoGen, beads' docs | Free. Unambiguous. The obvious candidate for the verb. |
| **Wisp**, **molecule**, **bead**, **rig**, **convoy** | beads | Do not import. Instructive prior art, unusable vocabulary. |

**The naming question this leaves open is the ticket-worthy one.** kestrel already has a noun for a unit of
work — a **Run**, and *"work that is queued but not yet started is a run in a queued state."* If the ledger
is a graph over *queued Runs*, kestrel needs **no new noun at all**: it needs a blocking edge between two
Runs and a rule for when a queued Run becomes eligible. That is the cheapest possible answer and it is
consistent with everything in `CONTEXT.md` — but it collides immediately with the invariant, because a
graph over queued Runs inside one Session is just a Workflow with dynamic edges, and a graph over queued
Runs across Sessions is a new organization-scoped entity that the one-active-run invariant does not
constrain at all. **Which of those two the ledger is, is the question #18 has to answer first**, and no
amount of prior art decides it.

---

## What this changes, concretely

1. **`Work` gains nothing; `Store` gains a graph.** Blocking edges and eligibility are kestrel's own rows.
   `Work.enqueue` is called at the moment a work item becomes ready, not when it is created. Six ports
   stand.
2. **The claim primitive is DBOS's, not Airflow's.** A conditional `UPDATE ... WHERE status = 'ready'
   ... RETURNING`, trusting only the flipped rows, with `FOR UPDATE SKIP LOCKED` as a Postgres-only
   accelerator. SQLite is the compose default and has no `SKIP LOCKED`; a design that requires it splits
   the required target set, which is [#4's](https://github.com/jtmthf/kestrel/issues/4) rule a fourth time.
3. **The fencing token moves, or is duplicated, onto the work item.** #15 put it on the run-held lease.
   Temporal puts it on the Activity Task Execution. A ledger needs the per-item form.
4. **Heartbeats are `Store` writes and never `Log` appends** — beads migrated exactly this and said why.
   The `Log` records `claimed` / `reclaimed` / `released`; the heartbeat cell is mutable state.
5. **`Timer` becomes correctness-critical.** Lease expiry is a due-time sweep on an at-least-once,
   imprecise, global tick. The minimum honest lease TTL is bounded below by the substrate's tick — a day on
   Vercel Hobby. Another entry for that adapter's Pro floor.
6. **The invariant needs Restate's shared-handler carve-out.** "At most one run active per session" should
   be stated as *at most one exclusive writer, with concurrent readers permitted* — otherwise an approval
   resolution arriving from Slack while a run holds the slot has no defensible place in the model.
7. **[#10's](https://github.com/jtmthf/kestrel/issues/10) unbounded session lifetime is unmatched by any
   surveyed system.** Temporal terminates a Workflow Execution past 51,200 history events and offers
   Continue-As-New; Restate retains journals for 24 hours by default; beads has `bd compact` and
   `bd prune`. kestrel promises unbounded lifetime with per-Organization retention as the escape hatch, and
   is the only one here doing so. That is a defensible choice, but it is a choice nobody else made.

---

## Coverage gaps — not examined

Stated so they are not mistaken for coverage.

- **Restate's server architecture.** The key-concepts page says *"The Restate Server stores all state and
  execution history"*; the partitioning, log and storage-engine details behind that were not read, and they
  are what would decide whether Restate could ever *be* kestrel's `Store` + `Log` rather than a comparand.
- **Temporal's matching service.** The claim path — how a task is handed to exactly one poller — was read
  only at the concept level. The actual dispatch, sticky queues, and what happens to an in-flight task on
  worker shutdown are unread.
- **beads' `bd swarm`, `bd cook` / fanout, and the `waits-for` gate.** The `waits-for` edge type
  (*"Fanout gate: wait for dynamic children"*) is the closest prior art to dynamic sub-work and it was read
  only through the `is_blocked` SQL, not through the commands that create it.
- **beads' federation and bucket-federation.** Read only as far as *"leases are node-local."* Whether a
  shared Dolt SQL server changes the concurrency answer (it should — one database, one lease table) was not
  tested.
- **DBOS's `notifications` and `streams` tables.** `streams` is a per-workflow append-only log with an
  `offset`, which looks like a direct analogue of kestrel's `Log`, and it was not read.
- **LangGraph Platform / LangGraph Server.** The self-hosted control plane, its queue, and its
  double-texting behaviour were not read; the OSS library was. If it has a claim/lease protocol it belongs
  in the ledger table, not the coordination one.
- **Lambda durable functions and Vercel Workflows** — flagged as unexamined by
  [#14](https://github.com/jtmthf/kestrel/issues/14) and still unexamined. Both overlap `Work` directly.
- **Cadence, Inngest, Trigger.dev, River, Oban, graphile-worker, Faktory.** The last four are the pure
  `SKIP LOCKED` queue family and would sharpen the portable-claim finding; none were read.
- **AutoGen's distributed runtime.** Only the single-process topic/subscription model was read. Whether the
  gRPC runtime adds any durability is unknown and would change its row in the addressing table.
- **A2A's push-notification authentication and task retention.** Read the state machine, not the security
  model or how long a completed Task remains addressable.
- **Nothing here was benchmarked.** Every claim about cost, scale or contention is the vendor's or the
  source comment's, not measured.
