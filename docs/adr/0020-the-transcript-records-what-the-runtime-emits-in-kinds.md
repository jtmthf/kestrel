# The Transcript records what the runtime emits, in kinds

A transcript records everything the agent runtime emits, in three kinds under one order and one
cursor: **shared state** (messages, run boundaries, participant joins, the branch and the pull
request, every approval and question resolved), **narration** (the agent's thoughts and plans), and
**detail** (tool calls and their results). A read names the kinds it wants. Shared state alone is
what a human gets when they join a session late.

This replaces `CONTEXT.md`'s previous definition, which recorded "what changed the session's shared
state, never what happened inside a run," and retires the supervisor test
`a_plan_a_tool_call_and_a_thought_are_heard_and_never_said` that enforced it.

## Why the old definition was rejected

It was not wrong about what a late joiner needs. It was wrong about where to enforce it. The
supervisor already receives every ACP `SessionNotification` and keeps two of them —
`AgentMessageChunk` and `UsageUpdate` — dropping `Plan`, `AgentThoughtChunk` and `ToolCall` at
`runtime.rs:389`. On 2026-09-09 an operator watched `run list` say `active` for twenty-eight minutes
while an agent thrashed on `openssl s_client` pipelines, and the only way to see it was to
`docker exec` into the instance and read opencode's own log file — past kestrel entirely, into a
private detail of a runtime kestrel deliberately knows nothing about.

The gap was worse than the dropped notifications suggested. `everything_left_to_say` is called only
after `work()` returns, so `Said`, `Model` and `Used` are all emitted at run end: between `Started`
and `Finished` the link carries `Heartbeat` and nothing else. Every one of that session's
twenty-seven `said` entries carries the same timestamp. A session was not readable in real time even
for the kind the old definition did record.

Keeping the transcript narrow and adding a second surface was considered and rejected: two surfaces
means two cursors, two retention rules, and a permanent question about which one to look in.
Filtering is a **read** concern, and it belongs on the read.

## Consequences

- **An entry is a completed unit, never a fragment.** A finished tool call, a finished message; never
  a delta. [ADR-0002](0002-two-deployables-the-environment-dials-out.md) builds reconnect on
  opencode's `?after=<seq>` replay and notes that the four `.delta` streaming events are the only
  ones it will not replay — safe then because the old definition excluded them by definition, and
  safe now because this one does too. The record stays gap-free, which is a promise
  `CONTEXT.md` protects hard enough to record its own deletions. Liveness is bounded by unit size,
  and a completed tool call arriving as it completes would have shown the `openssl` thrash
  immediately, which is all the operator wanted.
- **Reports go up as they happen**, rather than being drained after the turn. This is a bug against
  even the old definition, and it needs no new vocabulary to fix.
- **The never-expires promise tiers with the kinds.** It was justified by "a transcript is bounded by
  work kestrel chose to do," and 439 bash calls in one failed run is not that bound. Shared state
  keeps the promise it was made about; narration and detail have a retention window. The alternative
  — breaking the promise flat — would have made a session's narrative silently editable, which is the
  thing the promise exists to prevent.
- **Large payloads are references, not entries.** Tiering alone does not save you at 439 calls a run.
- **This is `0.3`.** "kestrel's work is joinable mid-flight — you pick up a running session instead of
  reading a finished one" is already on the ladder, and already names the client as where joining
  surfaces. This decision makes that rung bigger than `ROADMAP.md` currently describes; it does not
  add a rung.
