# Dynamic workflow declaration, fan-out, and fan-in

Resolves [#21](https://github.com/jtmthf/kestrel/issues/21). Primary docs read 2026-08-27/28. Source read at
`crewAIInc/crewAI@main` (`lib/crewai/src/crewai/crew.py`, `process.py`,
`tools/agent_tools/agent_tools.py`). Anthropic material read from `platform.claude.com`,
`code.claude.com` and `anthropic.com/engineering`; the rest from each vendor's own docs.

Companion to [`work-ledgers.md`](./work-ledgers.md), which answered #17. That page surveyed **claiming and
coordination**. This one answers the two questions it deliberately left alone: **how a process whose steps
are unknown at declaration time is declared**, and **how fan-out and fan-in are expressed**.

---

## The finding: kestrel's missing noun exists, and the shape kestrel proposed for it is the one shape the industry retreated from

Three findings, in descending order of how much they move #18.

**1. The missing noun is the *running instance*, and every system surveyed has one — including Anthropic's,
which uses kestrel's own two words and supplies the third.** Anthropic Managed Agents names the declared
thing an **Agent**, its running instance a **Session**, and the dynamically-created unit a **session
thread**. That is kestrel's `Agent` and `Session` in the *same roles* kestrel already uses them for —
except that kestrel's `Session` is not the running instance of anything, because kestrel's declared thing is
a `Workflow` and nothing runs it. The gap #18 exposed is real, it is one noun wide, and the industry
answer is not to invent a word: it is to notice that **the thing that runs a declaration and the thing that
holds a conversation are the same object.**

**2. The live proposal — a `Workflow` runs in a parent `Session`, some of its `Run`s spawn child `Session`s
with their own `Workspace` — is the one arrangement two systems built, shipped, and then backed out of.**

- **Airflow's SubDAG was exactly this shape**: the dynamic unit was a *full-fledged separate DAG run* with
  its own DAG attributes. Airflow deprecated it and **removed it entirely in 3.0**, replacing it with
  `TaskGroup`, where the tasks *"live on the same original DAG while honoring all the DAG settings and pool
  configurations."* The stated drawbacks read like a list of what would go wrong in kestrel: *"SubDAGs have
  their own DAG attributes. When the SubDAG DAG attributes are inconsistent with its parent DAG, unexpected
  behavior can occur"*; *"Unable to see the 'full' DAG in one view as SubDAGs exists as a full fledged
  DAG"*; *"Parallelism is not honored by SubDagOperator, and so resources could be consumed by
  SubdagOperators beyond any limits you may have set"*
  ([Airflow 2.6 DAGs](https://airflow.apache.org/docs/apache-airflow/2.6.0/core-concepts/dags.html)).
- **Anthropic's agent teams is the same shape in the agent literature** — *"One session acts as the team
  lead"* and teammates are *"Separate Claude Code instances"* — and it is **experimental, disabled by
  default**, and documents the precise failure kestrel's proposal would inherit
  ([agent teams](https://code.claude.com/docs/en/agent-teams)):

  > "**Subagents**: Claude receives the subagent's result when it completes. **Teammates**: the idle
  > notification reports that the teammate stopped, without its output."
  >
  > "An orchestration flow that waits on subagent results can stall."

  **Promoting the dynamic unit from a child-of-the-run to a peer session cost them the return value.** That
  is a fan-in failure caused by exactly the promotion #18 is contemplating, documented by the vendor, with
  the recommended fix being *"turn agent teams off."*

  Its `Limitations` section adds four more that map one-for-one onto kestrel's invariants: *"No session
  resumption with in-process teammates"*, *"One team per session"*, *"No nested teams: teammates cannot
  spawn their own teammates"*, *"Lead is fixed."*

**3. The half of the proposal that is right — a separate branch per concurrent unit — does not require a
child session, and Anthropic ships it without one.** A Claude Code subagent can carry
`isolation: worktree` in its frontmatter, giving it *"a separate working directory with its own files and
branch"* while remaining a subagent inside one session whose result returns as a tool result
([worktrees](https://code.claude.com/docs/en/worktrees)). The workflow runtime's own migration example is
*"transform each one in an isolated copy so edits don't conflict"*
([dynamic workflows](https://code.claude.com/docs/en/workflows)). **"Its own branch" and "its own session"
are independent axes, and only kestrel's glossary welds them together** — via the invariant *"A session
has exactly one workspace."*

So the fork #18 must settle is not (a)/(b)/(c). It is narrower and sharper:

- **Option W1 — the `Workspace` moves below the `Session`.** A `Run` may declare a workspace variant (a
  branch) of its session's workspace. Concurrency is across `Run`s in one session; the one-active-run
  invariant falls, and the ADR [#11](https://github.com/jtmthf/kestrel/issues/11) demanded is owed.
- **Option W2 — child `Session`s, as proposed.** The invariant survives per session. kestrel pays what
  Airflow and agent teams paid: results do not return, the parent must poll or be notified, and the
  "full workflow in one view" problem is a product problem from day one.
- **Option W3 — the dynamic unit is not a `Run` at all.** It is a *unit inside* one `Run` — the runtime's
  own subagent, behind the agent-runtime contract, invisible to kestrel. This is what
  [#2](https://github.com/jtmthf/kestrel/issues/2) already put behind the seam, and it costs kestrel
  nothing. It also buys kestrel nothing durable: the fan-out is not in `Store` or `Log`.

**W3 is free and W1 is honest; W2 is the one with prior art against it.** Nothing in this survey decides
between W1 and W3, because that choice is about what kestrel promises to *observe*, not about mechanism.

Two more findings that change something concrete:

**Nobody lets an orchestrator spawn an arbitrary undeclared unit. Every model-decides system declares a
roster.** Managed Agents: `multiagent.agents`, 1–20 entries, snapshotted at save time, *"The coordinator
can only delegate to one level of agents"* — enforced with a validation error. Agent SDK: an `agents` map.
CrewAI: an `agents` list. OpenAI Agents SDK: a `handoffs` list. ADK: `sub_agents`. **So a `Workflow` whose
steps are unknown is still not an empty declaration** — what it declares is the *set of agents its runs may
spawn*, and that set is where policy attaches. This is the concrete answer to #21's "what does the author
write."

**No system treats integration of divergent work products as a first-class step.** Not one. Merging is
always an ordinary downstream unit reading a shared place. The closest anything comes is Anthropic's
`/deep-research`, which *"cross-checks the sources it finds, votes on each claim"* — a *quality* pattern,
not a merge primitive — and Step Functions' `ToleratedFailurePercentage`, which is the only first-class
expression anywhere of *how many children may fail before the fan-in fails*. **kestrel has no word for that
threshold and will need one.**

---

## The naming survey

Every system needs three nouns: the **declared** thing, its **running instance**, and the
**dynamically-created unit**. This is the ticket's most valuable output, so it is stated twice — once as
the table, once as what the table means.

| System | Declared | Running instance | Dynamically-created unit | Unit is first-class? |
|---|---|---|---|---|
| **kestrel (today)** | `Workflow` | **— (the gap)** | `Run` | yes — durable, own environment |
| **Anthropic Managed Agents** | `Agent` (versioned, stored) | **`Session`** | **session `thread`** (+ `advisor` thread) | yes — own id, status, event stream, `parent_thread_id`, usage |
| **Claude Agent SDK / Claude Code subagents** | `AgentDefinition` (`agents` map or `.claude/agents/*.md`) | session (`session_id`) | subagent, spawned via the **`Agent` tool** (renamed from `Task` in v2.1.63) | partly — own `agentId`, own transcript file, resumable; but reached only as a tool result |
| **Claude Code dynamic workflows** | the **workflow script** (`meta.name`, saved in `.claude/workflows/`) | a **run** (`/workflows` lists runs) | an **agent**, from `agent()` / `pipeline()` | no — a script variable and a replay record |
| **Claude Code agent teams** | (a subagent definition, optionally) | the lead **session** | a **teammate** — *"Separate Claude Code instances"* | yes — a whole peer session; **this is kestrel's proposal** |
| **LangGraph (OSS)** | `StateGraph` / compiled graph | **thread** (`thread_id`, checkpoints) | a **task** created by `Send(node, state)` | no — a checkpointed entry in a superstep |
| **LangGraph Platform / Agent Server** | **assistant** — *"an instance of a graph with a specific configuration"* | **thread** — *"a persistent conversation container that maintains state across multiple runs"* | **run** — *"Runs combine an assistant and thread"* | yes — server-side, resumable |
| **CrewAI** | `Crew` + `Task` list + `Agent` list | a **kickoff** (`CrewOutput`) — *unnamed as a noun* | **none** — the manager delegates via a tool, creating no task | no — `DelegateWorkTool` returns a string |
| **OpenAI Agents SDK** | `Agent` | a **run** (`Runner.run` → `RunResult`); `Session` for history | **none** — handoff swaps the current agent inside one run | no |
| **Google ADK** | `Agent` (`SequentialAgent`, `ParallelAgent`, `LoopAgent`) | **`Session`** — *"a single conversation thread between a user and your agent"* | **branch** (a `ParallelAgent` sub-agent's execution branch) | no — and `sub_agents` is a **static list** |
| **Temporal** | **Workflow Definition** — *"The code that defines the constraints of a Workflow Execution"* / **Workflow Type** — *"A name that maps to a Workflow Definition"* | **Workflow Execution** (Workflow Id + **Run Id**) | **Child Workflow Execution** | **yes — the strongest case in the survey**: own Event History, own Run Id, own Parent Close Policy |
| **AWS Step Functions** | **state machine** (+ the *Map workflow*) | **execution** | **child workflow execution**, grouped by a **Map Run** | **yes** — *"Each child workflow execution has its own execution history, which is separate from the parent workflow's execution history"*; the Map Run has its own ARN |
| **Airflow** | **DAG** (+ a **mapped task**) | **DagRun** | **mapped task instance** | no — a `TaskInstance` row with a map index |
| **Airflow (removed)** | **SubDAG** | a nested **DagRun** | — | yes — *and removed in 3.0* |
| **Prefect** | **flow** | **flow run** — *"A representation of a single invocation of a flow"* | **child flow run** / task run | **yes** — *"There is a full representation of the nested flow run in the backend as if it had been called separately"* |
| **Dagster** | **job** / **graph** / **op** | **run** | a **cloned op** per `DynamicOutput`, keyed by `mapping_key` | no — a step in one run's execution plan |
| **Inngest** | **function** (`inngest.createFunction`) | **function run** | **step** (`step.run`), or an invoked **function run** (`step.invoke`) | step: no. `step.invoke`: yes — *"runs with its own configuration, distinct from the invoker's"* |
| **Restate** | **Service** / **Virtual Object** / **Workflow** | **invocation** (journal) | a nested **invocation** (call or `.send()`) | yes — its own journal and invocation id |

### What the table means for kestrel

**kestrel's `Run` is in the third column, and the third column is the *cheap* one almost everywhere.** In
Airflow, Dagster, LangGraph and Inngest the dynamic unit is a row or a step inside the parent's execution —
not an instance of the declared type. In Temporal, Step Functions, Prefect and Restate it *is* a
first-class instance, and all four are systems where the declared thing is *a program*, so "child" costs
nothing conceptually: a program calling a program is still a program.

**kestrel is in the second group by construction** — a `Run` is *"One execution of an agent runtime inside
one environment"*, which is heavyweight by definition — but it is missing the second column, which is what
the first group would have called the parent. That is the whole of #18's naming problem, stated
positionally.

**The three candidate resolutions, in the glossary's own terms:**

1. **`Session` becomes the running instance of a `Workflow`.** This is Managed Agents' assignment exactly,
   and it is nearly free: `CONTEXT.md` already says a `Workflow` is *"executing within a single session"*.
   What it costs is that a session is currently started by a `Trigger`, not by a `Workflow`, and sessions
   exist with no workflow at all. So the relation is *may have*, not *is an instance of* — and a
   `Trigger`:`Event` :: `Workflow`:`Run` analogy that skips a rung is what left the hole.
2. **A new noun for the running instance.** The survey offers **Execution** (Temporal, Step Functions),
   **Invocation** (Restate), **Run** (Prefect, Dagster, LangGraph Platform, Inngest) and **Thread**
   (LangGraph OSS, Managed Agents, ADK). `Run` is taken. `Thread` is on `CONTEXT.md`'s *avoid* list for
   `Session`. **`Execution` is free and unambiguous** — and it is the only one of the four that no other
   kestrel term avoids.
3. **No new noun: the `Workflow` *is* the declaration and the `Session` carries its progress.** Cheapest,
   and the one the glossary can hold today. It fails the moment two workflows run in one session, and
   nothing in `CONTEXT.md` currently forbids that.

---

## Three ways a process declares steps it does not know

Every system surveyed does one of exactly three things. The distinction matters because kestrel's
`Workflow` is currently in family A and the proposal moves it toward C.

### A — the *shape* is declared, the *cardinality* is runtime

The author writes one body and one collection expression; the engine instantiates the body once per item.
This is the overwhelming majority, and it is the only family where the declaration is still statically
readable.

- **Airflow `expand`**: *"Dynamic Task Mapping allows a way for a workflow to create a number of tasks at
  runtime based upon current data, rather than the Dag author having to know in advance how many tasks would
  be needed"*, and *"dynamic task mapping defers task creation until runtime, allowing the scheduler to
  determine the exact number of task instances based on upstream task outputs"*
  ([dynamic task mapping](https://airflow.apache.org/docs/apache-airflow/stable/authoring-and-scheduling/dynamic-task-mapping.html)).
  Capped: `max_map_length`, default **1024**; exceeding it *fails the upstream source task*.
  **Limits worth recording**: mapping over a mapped task is prohibited, and mapping cannot be nested inside
  mapped task groups. So the fan-out is one level, like Managed Agents'.
- **Step Functions Distributed Map**: *"In Distributed mode, the `Map` state processes the items in the
  dataset in iterations called child workflow executions."* Default concurrency **10,000**; the recommended
  trigger is *"The workflow's execution event history would exceed 25,000 entries"* — i.e. **the fan-out
  exists to keep the parent's history small**, the same reason Temporal gives for child workflows.
- **Dagster `DynamicOut` / `DynamicOutput`**: *"the number of outputs is not known until runtime"*;
  downstream ops are *"cloned for each dynamic output, and identified using the associated mapping_key"*;
  and the type system forces the join — *"Either `map` or `collect` must be invoked on it."*
- **LangGraph `Send`**: exists for the case where *"the exact edges are not known ahead of time and/or you
  may want different versions of `State` to exist at the same time"*, returned from a **conditional edge**,
  and *"takes two arguments: first is the name of the node, and second is the state to pass to that node"*
  ([graph API](https://docs.langchain.com/oss/python/langgraph/graph-api)).
- **Inngest**: `chunks.map((chunk, i) => step.run(...))` then `Promise.all`. Capped at **1,000 steps** per
  function and **4 MB** of total step data.

### B — the plan is a program

The declaration is code; the branching is `if`. Temporal, Restate, Prefect, Inngest and — the interesting
one — Anthropic's dynamic workflows all live here.

- **Temporal**'s Workflow Definition is *"The code that defines the constraints of a Workflow Execution"*,
  and a **Dynamic Workflow** is a distinct, narrower thing: *"A Workflow that is invoked dynamically at
  runtime if no other Workflow with the same name is registered"*, registered with
  `worker.RegisterDynamicWorkflow()`, **one per Worker**, taking *"a single argument of type
  `converter.EncodedValues`"*
  ([dynamic workflow](https://docs.temporal.io/develop/go/workflows/dynamic-workflow)). **This is not what
  #21 means by dynamic** — it is a catch-all handler for unregistered types, useful for a generic runner,
  and worth naming so it is not mistaken for the answer.
- **Anthropic's Workflow tool** is the sharpest instance in the survey, because the *model writes the
  program*: *"A dynamic workflow is a JavaScript script that orchestrates subagents at scale. Claude writes
  the script for the task you describe, and a runtime executes it in the background while your session
  stays responsive."* The script body is plain JavaScript with top-level `await`; `agent()` spawns one
  subagent and `pipeline()` runs one per item. It is *saved as a command* afterwards — so the artifact of a
  dynamic run is a **new declaration**.

  Its own comparison table is the cleanest statement of the tradeoff anywhere, and it is the table #18
  should be judged against:

  | | Subagents | Skills | Agent teams | Workflows |
  |---|---|---|---|---|
  | What it is | A worker Claude spawns | Instructions Claude follows | A lead agent supervising peer sessions | A script the runtime executes |
  | Who decides what runs next | Claude, turn by turn | Claude, following the prompt | The lead agent, turn by turn | The script |
  | Where intermediate results live | Claude's context window | Claude's context window | A shared task list | Script variables |
  | Scale | A few delegated tasks per turn | Same as subagents | A handful of long-running peers | Dozens to hundreds of agents per run |
  | Interruption | Restarts the turn | Restarts the turn | Teammates keep running | Resumable in the same session |

  **"A lead agent supervising peer sessions" is kestrel's live proposal, and it is the column that scales
  worst** — *"a handful of long-running peers."*

### C — the model decides, against a declared roster

- **Anthropic, "Building effective agents"** draws the line kestrel's glossary needs:
  **Workflows** are *"Systems where LLMs and tools are orchestrated through predefined code paths"*;
  **Agents** are *"Systems where LLMs dynamically direct their own processes and tool usage, maintaining
  control over how they accomplish tasks."* On **orchestrator-workers**: *"a central LLM dynamically breaks
  down tasks, delegates them to worker LLMs, and synthesizes their results"*, and the distinguishing
  feature is that *"subtasks aren't pre-defined, but determined by the orchestrator based on the specific
  input."* On **parallelization** (sectioning/voting), the subtasks *are* predetermined — so
  **parallelism and dynamism are orthogonal axes**, which #18 has been treating as one.
- **Managed Agents** is the mechanism behind that framing, and it is worth reading precisely because it
  contradicts the proposal on the point that matters most:

  > "All agents share the same sandbox, filesystem, and vault credentials, but each agent runs in its own
  > **session thread**, a context-isolated event stream with its own conversation history. The coordinator
  > reports activity in the **primary thread** ...; **additional threads are spawned at runtime when the
  > coordinator delegates work.**"

  **One sandbox, one filesystem, N isolated conversations.** kestrel's proposal is the mirror image — N
  workspaces, N sessions. Anthropic isolates *context* and shares *state*; the proposal isolates *state*
  and (implicitly) shares nothing. The delegation tools are `list_agents` and `send_to_agent`, and *"Tools,
  MCP servers, and context are not shared"*, so **every delegated unit must carry its own paths and
  constraints in its brief** — the docs say so outright: *"Threads share the filesystem, not each other's
  conversation."*
- **CrewAI's hierarchical process creates no work at runtime, and the source says so in four lines.**
  `crew.py`:

  ```python
  def _run_hierarchical_process(self) -> CrewOutput:
      """Creates and assigns a manager agent to complete the tasks."""
      self._create_manager_agent()
      return self._execute_tasks(self.tasks)
  ```

  `_run_sequential_process` is `return self._execute_tasks(self.tasks)` — **the same task list**. What
  hierarchical changes is *who executes*: `executing_agent = self.manager_agent if is_hierarchical else
  task.agent`. The manager gets `AgentTools(agents=self.agents).tools()`, which is exactly two tools —
  `DelegateWorkTool` and `AskQuestionTool` — and a `Process` enum with two members and a `# TODO:
  consensual`. **CrewAI's "dynamic" fan-out is a tool call that returns a string; no unit of work is
  created, nothing is recorded, and nothing is durable.** Do not cite CrewAI as prior art for dynamic
  declaration.
- **OpenAI Agents SDK** does not have a dynamic unit at all. A handoff is *"we update the current agent and
  input, and re-run the loop"* — one run, one current agent, which is
  [#17](https://github.com/jtmthf/kestrel/issues/17)'s baton finding restated. For fan-out the docs point
  *out of the SDK*: *"Running multiple agents in parallel, e.g. via Python primitives like
  `asyncio.gather`"*, and `Agent.as_tool()`. **The SDK's own guidance is that code, not the model, should
  own orchestration where determinism matters** — *"makes tasks more deterministic and predictable, in terms
  of speed, cost and performance."*
- **Google ADK contradicts the ticket's premise outright**: `ParallelAgent(sub_agents=[...])` is a
  **static list**, and *"Each sub-agent operates in its own execution branch. There is no automatic sharing
  of conversation history or state between these branches during execution"*
  ([ParallelAgent](https://adk.dev/agents/workflow-agents/parallel-agents/)). ADK has isolated branches and
  no dynamic cardinality — the exact inverse of Airflow. Its running instance is a **`Session`**: *"a single
  conversation thread between a user and your agent."*

**The finding across C:** the roster is always declared, always small, and always one level deep. Managed
Agents caps it at 20 entries and **25 concurrent threads**; the Agent SDK defaults to depth **3** and
**20 concurrent** subagents (`CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH`, `CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS`)
plus a dollar cap (`maxBudgetUsd`); the workflow runtime caps at **16 concurrent agents** and **1,000 agents
per run**. Every one of these is a *cardinality* cap, not a priority scheme — see the #10 note below.

---

## Fan-in: where the children's outputs live, and who reads them

| System | Where a child's output lands | Who reads it | Blocking mechanism |
|---|---|---|---|
| **Managed Agents** | an `agent.thread_message_received` event on the coordinator's thread | the coordinator, in a later turn | none — *"Spawning returns immediately; the subagent's report arrives in a later coordinator turn"* |
| **Agent SDK subagents** | the **`Agent` tool result** — *"only its final message returns to the parent"* | the parent model | tool-call semantics; background by default since v2.1.198, `run_in_background: false` to block |
| **Claude Code workflows** | **script variables** (`await agent(...)`, `await pipeline(...)`) | the script | JavaScript `await` |
| **Claude Code agent teams** | **a shared task list** at `~/.claude/tasks/{team}/`, or a direct message | the lead, or any teammate | **none — and this is the bug**: *"the idle notification reports that the teammate stopped, without its output"* |
| **LangGraph** | **state channels**, combined by a per-key **reducer** (`Annotated[list, operator.add]`) | the downstream node | **superstep**: *"Nodes that run in parallel are part of the same super-step"*; execution halts when *"all nodes are inactive and no messages are in transit"* |
| **CrewAI** | a `TaskOutput` appended to `task_outputs` | the next task, via `Task.context` | the next **synchronous** task is the barrier — `if futures: task_outputs.extend(self._process_async_tasks(...))` before `execute_sync` |
| **Google ADK** | `session.state`, keyed by the sub-agent's **`output_key`** | a later agent, via `{key}` interpolation in its instruction | the `SequentialAgent` that follows the `ParallelAgent` |
| **OpenAI Agents SDK** | a `RunResult` per `asyncio.gather` branch, or a tool result from `as_tool()` | the caller's code, or the manager agent | `asyncio.gather` |
| **Temporal** | the Child Workflow Execution's result | the parent | *"A Parent Workflow Execution must await on the Child Workflow Execution to spawn. The Parent can optionally await on the result"* |
| **Step Functions** | **`ResultWriter` → S3**, or *"an array of all the child workflow execution results"* | the next state | the Map Run completes |
| **Airflow** | XCom, surfaced to the reduce task as a **lazy sequence** | the downstream task | trigger rules over all map indices |
| **Dagster** | `.collect()` — *"creates a fan-in dependency over all the dynamic copies"* | the collecting op | the type system refuses anything else |
| **Prefect** | the child flow run's return value | the calling flow | the call |
| **Inngest** | step results | the function body | `Promise.all` |
| **Restate** | the nested invocation's result | the caller | `RestatePromise.all` / `.any` / `.race` / `.allSettled` |

Three things fall out.

**Every fan-in is a downstream unit reading a shared place. There is no merge primitive anywhere.** Even
where the work products diverge physically — Claude Code's worktrees, one branch per subagent — the
integration is left to whoever asks for it: *"a worktree with changes stays on disk until the periodic sweep
below can remove it without losing work."* **No engine merges branches, and none offers a step type for
it.** If kestrel wants integration-of-divergent-work as a named step, it will be inventing it, not
adopting it. That is not a reason not to; it is a reason to expect no prior art to lean on.

**Only two systems express partial failure of a fan-out.** Step Functions has `ToleratedFailureCount` and
`ToleratedFailurePercentage`, with *"The default percentage value is zero, which means that the workflow
fails if any one of its child workflow executions fails or times out"*, and a dedicated error,
`States.ExceedToleratedFailureThreshold`. Restate has `RestatePromise.allSettled`. Everyone else is
all-or-nothing, and Anthropic's `pipeline()` is honest about the consequence: *"An `agent()` call resolves
to `null` if you stop it mid-run or it hits an unrecoverable API error. `pipeline()` keeps that `null` in
the results array"* — the author writes `.filter(Boolean)`. **kestrel's `Workflow` promise from
[#10](https://github.com/jtmthf/kestrel/issues/10) — "conditional continuation on exit status" — is
defined for one predecessor. It has no meaning over twelve, and this is where it breaks first.**

**Fan-out resumption is where the script-as-plan model pays.** Anthropic states it plainly:

> "**Failed**: runs again, and so does every agent that started after it, even ones that completed. ...
> That last case means a failure in the middle of a fan-out reruns work that already finished. If a script
> starts A, B, C, and D in that order and B fails, relaunching returns A from cache and runs B, C, and D
> again."

Temporal, Step Functions and Prefect do not have this problem *because* the dynamic unit is a first-class
durable entity — that is the whole benefit of the first column of the naming table being expensive. **This
is the strongest argument in the survey for kestrel's dynamic unit staying a `Run`**, i.e. staying durable,
whatever the containing noun turns out to be.

---

## Temporal, as the primary comparison

The ticket asked for Temporal's parent/child model as the closest analogue. It is closest, and it is closest
in a way that argues against the proposal rather than for it.

**Children are unambiguously first-class.** *"A Child Workflow Execution in the Temporal platform is
initiated from another Workflow within the same Namespace"*, and *"Because Child Workflow Executions have
their own Event Histories, they are often used to partition large workloads into smaller chunks"*
([child workflows](https://docs.temporal.io/encyclopedia/child-workflows)). That is the kestrel proposal's
strongest support: a child that owns its own record is a real thing, and Temporal, Step Functions, Prefect
and Restate all build it.

**And Temporal tells you not to reach for it.** In its own words:

> "There is no reason to use Child Workflows just for code organization."
>
> "When in doubt, use an Activity."

The listed reasons to use one are all *structural*: separate services processed by different Workers,
partitioning Event Histories, one-to-one resource mappings, periodic logic via Continue-As-New. **Not one
of them is "the child needs its own state" or "the child needs to be independently joinable."** kestrel's
proposal rests on both.

**`ParentClosePolicy` is the mechanism kestrel has no analogue for and would need.**
*"A Parent Close Policy determines what happens to a Child Workflow Execution if its Parent changes to a
Closed status (Completed, Failed, or Timed out)"*
([parent close policy](https://docs.temporal.io/parent-close-policy)), with three values:

- **Abandon** — *"the Child Workflow Execution is not affected."*
- **Request Cancel** — *"a Cancellation request is sent to the Child Workflow Execution."*
- **Terminate** — *"the Child Workflow Execution is forcefully Terminated."* **This is the default.**

Under the live proposal, a child `Session` that outlives its parent is a joinable session with a workspace,
a transcript and a branch, whose reason for existing has closed. **The default in the industry's canonical
implementation is to kill it** — and kestrel's `Session` is defined as *"durable, joinable"* and *"survives
restarts"*, which is Abandon by construction. **A child-session model owes a close policy, and kestrel's
glossary currently hard-codes the least safe of the three.** This is the single most concrete thing #21
hands #18.

**The fan-out ceiling.** *A single Parent should not spawn more than 1,000 Child Workflow Executions*,
because the parent's Event History carries an event per child status change. **The parent's log is the
scaling limit on fan-out** — which is the same reason Step Functions gives (*"execution event history would
exceed 25,000 entries"*) and the same shape as [#14](https://github.com/jtmthf/kestrel/issues/14)'s `Log`.
[#17](https://github.com/jtmthf/kestrel/issues/17) already flagged Temporal's 51,200-event termination
against kestrel's unbounded session lifetime; **fan-out is the feature that makes that flag urgent**,
because every child transition is a parent Transcript event.

---

## Judging every system against the live proposal

The proposal: a `Workflow` executes in a **parent `Session`**; its steps are `Run`s in that parent; some
`Run`s **spawn child `Session`s**, each with its own `Workspace` and therefore its own branch; a later `Run`
in the parent integrates their outputs.

| System | Verdict |
|---|---|
| **Anthropic Managed Agents** | **Contradicts on state.** Same three-noun shape, opposite isolation: *"All agents share the same sandbox, filesystem, and vault credentials"*, isolating context instead. One level of delegation, enforced. |
| **Claude Agent SDK subagents** | **Contradicts on containment.** The dynamic unit lives *inside* the run and returns as a tool result. Supplies the branch-per-unit half via `isolation: worktree` **without** a child session. |
| **Claude Code dynamic workflows** | **Contradicts on where the plan lives.** The plan is a script outside the conversation; intermediate results are *"Script variables"*, deliberately not context. |
| **Claude Code agent teams** | **Matches — and is the cautionary tale.** Peer sessions, own context, shared task list. Experimental, off by default, no nesting, no resumption, and **fan-in loses the result**. |
| **Temporal** | **Matches structurally, warns against use.** *"When in doubt, use an Activity."* Supplies `ParentClosePolicy`, which the proposal is missing. |
| **Step Functions Distributed Map** | **Matches, at data scale.** Child executions with their own histories, grouped by a Map Run; but the *body* is declared and only the cardinality is dynamic. |
| **Prefect** | **Matches most cleanly.** *"There is a full representation of the nested flow run in the backend as if it had been called separately"*, and *"each child flow run is linked to its parent and can be individually observed."* The parent/child model with none of Airflow's SubDAG problems — because a flow run has no workspace. |
| **Restate** | **Matches structurally.** Nested invocations with their own journals; per-key exclusivity as in [#17](https://github.com/jtmthf/kestrel/issues/17). |
| **Inngest** | **Split.** `step.run` contradicts (a step is not an instance); `step.invoke` matches — *"runs with its own configuration, distinct from the invoker's."* |
| **Airflow (`expand`)** | **Contradicts.** The dynamic unit is a `TaskInstance` row, not an instance of the declared type. |
| **Airflow (SubDAG)** | **Matched, then removed.** See the finding above. |
| **Dagster** | **Contradicts.** A cloned op in one run's plan; `map`/`collect` are compile-time constraints. |
| **LangGraph** | **Contradicts.** `Send` creates a task in a superstep, not an instance. Its Platform layer restores the three nouns (assistant/thread/run) but not parent/child. |
| **CrewAI** | **Contradicts, and creates nothing.** Hierarchical execution runs the declared `self.tasks`. |
| **OpenAI Agents SDK** | **Contradicts.** One run, one current agent; fan-out is `asyncio.gather` outside the model. |
| **Google ADK** | **Half-matches.** Isolated branches — *"no automatic sharing of conversation history or state"* — but `sub_agents` is static, so nothing is dynamic. |

**Score: two systems built the proposal (Airflow SubDAG, agent teams). One removed it; the other ships it
experimental, disabled by default, with a documented fan-in failure.** Four more (Temporal, Step Functions,
Prefect, Restate) build parent/child instances and are all systems where the running instance is *a program
with no place attached* — no workspace, no branch, no filesystem identity. **The variable that separates
the safe cases from the regretted ones is whether the child owns a place.** kestrel's child `Session` owns a
`Workspace`, which puts it on the wrong side of that line.

---

## Two settled decisions, tested

**[#10](https://github.com/jtmthf/kestrel/issues/10) excluded priority scheduling. Nothing here needs it,
and the survey is unanimous.** Every dynamic fan-out surveyed is bounded by a **cardinality cap**, never by
priority: Step Functions `MaxConcurrency` (default 10,000), Airflow `max_map_length` (1024), Temporal ~1,000
children, Inngest 1,000 steps, Managed Agents 20 roster entries / 25 concurrent threads, Agent SDK depth 3 /
20 concurrent / `maxBudgetUsd`, the workflow runtime 16 concurrent / 1,000 agents. **Not one of these is a
priority scheme.** What #10 should notice is that it did not exclude *these*, and a dynamic fan-out is
unusable without at least a concurrency cap and a spend cap — Anthropic ships both, plus an advisory
*"Large workflow"* warning at 25 agents or 1.5 M projected tokens.

**[#10](https://github.com/jtmthf/kestrel/issues/10)'s workflow floor — "declared sequence, conditional
continuation on exit status, and a human-approval step" — does not survive fan-out, and one clause fails
outright.** *"No mid-run user input"* is a hard constraint of Anthropic's workflow runtime: *"Only agent
permission prompts can pause a run. For sign-off between stages, run each stage as its own workflow."*
kestrel's approval step is mid-run user input by definition. **Either kestrel's fan-out permits an approval
inside a fanned-out unit — which no surveyed system does — or a workflow with a human-approval step cannot
fan out, and the two v1 workflow features do not compose.** That is a promise-level conflict, not a detail.

---

## Vocabulary: where kestrel needs a different noun

Extends the table in [`work-ledgers.md`](./work-ledgers.md); only new collisions listed.

| Prior art's word | Used by | kestrel's problem |
|---|---|---|
| **Thread** | Managed Agents (*session thread* — the dynamic unit), LangGraph (the *running instance*), ADK | **Banned as `Session`'s avoid-word**, and it names two different rungs in two Anthropic products. Unusable. |
| **Session** | Managed Agents (running instance), ADK (running instance), OpenAI Agents SDK (conversation history), Claude Code (a whole instance) | **Taken, and taken compatibly** — every one of these is a durable conversation container. Good news; the risk is that Anthropic's `Session` *is* the running instance and kestrel's is not. |
| **Agent** | Managed Agents (declared, versioned), Agent SDK (`AgentDefinition`), the Workflow tool (`agent()` — **the dynamic unit**) | Taken, and `CONTEXT.md`'s definition matches the first two exactly. But Anthropic's `agent()` names a *spawned unit*, which is kestrel's `Run`. Flag on import. |
| **Workflow** | Anthropic (a JS script the model writes), Temporal (a program), Restate (an exactly-once handler), Inngest (a function) | Taken, and kestrel's *"declared multi-step process"* is the **narrowest** meaning of the four. Everyone else's workflow is Turing-complete. Any borrowed reasoning about "dynamic workflows" is reasoning about programs. |
| **Execution** | Temporal (Workflow Execution), Step Functions (execution) | **Free.** The obvious candidate for the missing running-instance noun, and the only unclaimed one. |
| **Invocation** | Restate | On the `Run` avoid list (*"job, task, execution, invocation"*) — note this also bans **execution**, which is the best available candidate. **Reopening that one word is cheaper than inventing a new one.** |
| **Coordinator** / **Lead** / **Manager** | Managed Agents, agent teams, CrewAI | Free. But all three name an *agent*, not a run — the role is a property of the `Agent`, not a new entity. |
| **Roster** | Managed Agents (`multiagent.agents`) | **Free, and needed.** The declared set of agents a workflow's runs may spawn has no kestrel name and is the load-bearing part of the declaration. |
| **Fan-out** / **fan-in** | universal | Free as description. Neither is an entity. |
| **Branch** | git, ADK (*execution branch*), Airflow (*branching*) | Already used by kestrel's `Workspace` for the git sense. ADK's *"execution branch"* would collide; do not import. |
| **Map** / **Map Run** | Step Functions, Dagster, Airflow | Free but data-shaped; kestrel's fan-out is not over a dataset. |
| **Task list** | agent teams | `task` is banned. The shared work list has no kestrel name — but per #17, it may not need one: it is a graph over queued `Run`s. |

**The naming question this leaves open, sharpened.** #17 ended by asking whether the ledger is a graph over
queued `Run`s inside one `Session` or across sessions. This page narrows it: **the answer depends entirely
on whether a `Run` may declare its own workspace variant.** If it may, everything is one session and
kestrel needs one noun (the running instance) and one relation (blocking edges between queued `Run`s). If it
may not, kestrel needs child sessions, a close policy, a cross-session join, and an answer to the agent-teams
fan-in failure. **That is one bit, and it decides four tickets.**

---

## What this changes, concretely

1. **The missing noun is the running instance, and `Execution` is the only free candidate** — but it sits on
   `Run`'s avoid list. Reopening one avoid-word with a reason is cheaper than coining a term, and cheaper
   than the alternative of promoting `Session` to be the running instance of a `Workflow` when sessions
   already exist without workflows.
2. **A `Workflow` declares a roster, not a step list.** Every model-decides system in the survey declares
   the *set of agents* a runtime may spawn, and none permits spawning outside it. This is where kestrel's
   `Policy` attaches — an orchestrator run's grant is the intersection over its roster — and it makes
   "steps not known at declaration time" compatible with governance rather than opposed to it.
3. **A child-session model owes a close policy, and kestrel's default is currently Abandon.** Temporal's
   default is Terminate. Under the proposal a child `Session` is durable and joinable by definition, so its
   parent closing leaves it alive with no owner. Name the policy or drop the child.
4. **Every child transition is a parent Transcript event, and that is the fan-out ceiling.** Temporal caps
   at ~1,000 children for exactly this reason; Step Functions' documented trigger for Distributed Map is a
   25,000-entry history. This turns [#17](https://github.com/jtmthf/kestrel/issues/17)'s note about
   kestrel's unbounded session lifetime from a curiosity into a constraint on how wide a fan-out may be.
5. **Fan-out needs a failure threshold, and kestrel has no word for it.** [#10's](https://github.com/jtmthf/kestrel/issues/10)
   *"conditional continuation on exit status"* is defined over one predecessor. Step Functions'
   `ToleratedFailureCount` / `ToleratedFailurePercentage` is the only first-class prior art; the default
   there is zero — *"the workflow fails if any one of its child workflow executions fails"* — which is a
   defensible default for kestrel too, but it must be stated.
6. **Fan-out and the human-approval step do not compose, on current evidence.** Anthropic's runtime forbids
   mid-run user input in a workflow outright and tells you to split stages. Both are v1 workflow promises in
   #10. Settle which one bends.
7. **"Own branch" does not imply "own session."** `isolation: worktree` on a Claude Code subagent is the
   existence proof. If kestrel wants concurrent branches, the cheap move is to let a `Run` name a workspace
   variant — which breaks *"A session has exactly one workspace"*, an invariant far cheaper to amend than
   the one-active-run invariant is.
8. **Integration of divergent work products has no prior art as a step type.** Every engine leaves the merge
   to the author. If kestrel makes it first-class, that is a genuine differentiator and a genuine
   unknown — and it should be recorded as an invention, not a borrowing.
9. **Do not cite CrewAI's hierarchical process as dynamic declaration.** It executes `self.tasks`. The
   source is four lines and is quoted above.

---

## Coverage gaps — not examined

- **Anthropic's Workflow tool schema.** The `agent()` and `pipeline()` options beyond the saved-script
  example were not read; the Agent SDK TypeScript reference page did not surface a Workflow section on
  fetch. Whether `agent()` takes an isolation/worktree option — which would settle finding 7 outright — is
  **unknown and is the largest single gap on this page.**
- **Managed Agents' `agent_toolset_20260401` wire shapes.** `send_to_agent` and `list_agents` were read
  through the docs' description, not their schemas. What a delegated brief may carry, and whether a
  coordinator can address a thread it did not create, are unread.
- **Temporal's `ChildWorkflowOptions` beyond `ParentClosePolicy`** — Workflow Id reuse for children,
  cancellation propagation, and how a parent awaits *many* children (`Promise.allSettled` equivalents per
  SDK) were not read. The encyclopedia page does not state it.
- **Step Functions `ItemBatcher`, `ItemReader`, and `ResultWriter` output layout.** Read as field names
  only; the actual S3 manifest format that a fan-in step would have to parse is unread.
- **Airflow 3.0's release notes themselves.** The SubDAG removal is corroborated from the 2.6 docs'
  deprecation notice plus release-notes search results, not from the 3.0 changelog line item. The
  *rationale* is quoted from the 2.6 docs and is solid; the exact removal version is one hop weaker than
  the rest of this page.
- **Prefect's `.map()` and task-runner concurrency**, and whether a mapped task run is a first-class run
  like a child *flow* run. Only the flow/child-flow distinction was read.
- **LangGraph subgraphs**, which are the actual analogue of parent/child (a node that is itself a graph),
  and the Platform's run/thread API. `Send` was read; subgraphs were not.
- **Inngest `step.invoke` semantics.** The docs page returned only that an invoked function *"runs with its
  own configuration, distinct from the invoker's"*; run identity, timeout, and cancellation propagation are
  unread, so Inngest's row in the first-class column is weaker than Temporal's or Prefect's.
- **Restate's Awakeables and durable promises**, which are its fan-in primitive for work completed outside
  the caller. Only `RestatePromise.*` combinators were read.
- **ADK's `InvocationContext` and whether ADK has any dynamic-cardinality mechanism at all.** Only
  `ParallelAgent` was read; if ADK has a `Send` equivalent it was not found.
- **AutoGen, Trigger.dev, Windmill, Hatchet, Cadence, Vercel Workflows, AWS Lambda durable functions** —
  none read. The last two overlap `Work` directly and were already flagged unexamined by
  [#14](https://github.com/jtmthf/kestrel/issues/14) and #17; they are still unexamined.
- **Nothing here was run.** Every limit, cap and default is the vendor's stated number, not a measured one.
