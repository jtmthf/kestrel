# What ACP runtimes emit within a turn

Resolves [Research what ACP runtimes emit within a turn](https://github.com/openkestrel/kestrel/issues/297).
It supplies the facts for
[Decide what a completed unit is for each thing a runtime emits](https://github.com/openkestrel/kestrel/issues/309),
which owns the rule this note does not set. The question matters because of
[ADR-0020](../adr/0020-the-transcript-records-what-the-runtime-emits-in-kinds.md): a Transcript
entry records a completed unit and never a fragment, so kestrel has to know where each kind of
update ends.

Read on 2026-09-26, from source rather than prose about it:

- The ACP specification and schema at [`322cb37`][acp] (`main`). kestrel locks
  `agent-client-protocol-schema` 1.5.0. Since that release, the v1 pages cited here have changed
  only in details this note does not rely on: the tool-call `name` field, `rawInput` typing, a
  `switch_mode` kind, and a table of `session/update` variants.
- opencode [`v2.0.14`][oc], the version `images/kestrel-env/Dockerfile` pins. Its ACP agent lives
  in `packages/cli/src/acp`.
- `@agentclientprotocol/claude-agent-acp` [`v0.79.0`][cl] and `@agentclientprotocol/codex-acp`
  [`v1.12.0`][cx], the versions `images/kestrel-dev/package.json` pins. codex-acp 1.12.0 is a
  TypeScript adapter over the Codex App Server; see
  [subscription-auth-acp.md](subscription-auth-acp.md) for why Zed's older Rust adapter is no
  longer the one to read.

Every claim about an adapter holds for that version only. Nothing here was captured from a live
run.

kestrel sends `initialize` with default client capabilities ([`runtime.rs` L149][ks-init], [L417][ks-init2]).
Some adapter features switch on only when a client declares an extension in `_meta`: typed
session failures, `terminal_output`, subagent transcripts, opencode's child-session updates and
Codex's `plan_update`. kestrel declares none of them. This note describes what a client like that
receives.

## Headline

**ACP v1 signals the end of nothing inside a turn except, weakly, a tool call.** Messages,
thoughts and plans have no completion signal. A message is over only when a chunk with a
different `messageId` arrives or the `session/prompt` response returns. A plan arrives as a
series of whole snapshots, and none is marked final. A tool call has terminal statuses, but the
specification never requires an Agent to send one. On `session/cancel` it gives the Client the
job of closing open calls. The `session/prompt` response is the only boundary every runtime must
produce.

**The three runtimes differ at every one of those boundaries:**

| | opencode 2.0.14 | Claude Code (claude-agent-acp 0.79.0) | Codex (codex-acp 1.12.0) |
|---|---|---|---|
| A message's `messageId` spans | one model step | one Anthropic API message | one Codex `agentMessage` item |
| A thought's `messageId` | its own, one per reasoning part | shared with the text of the same API message | its own, one per reasoning item |
| Tool output streams | no | no | yes, but only in `_meta` |
| Open tool calls reach `completed`/`failed` on cancel | yes | no | only if Codex reports them (not verified) |
| `plan` | never sent | whole snapshot on each TodoWrite or Task* change | whole snapshot on each `update_plan` |
| A failed turn ends as | JSON-RPC error | JSON-RPC error | usually `end_turn`, with the error sent as message text |

Sources for each cell are in the sections below.

## What the specification guarantees

### Messages

`agent_message_chunk` is "A streamed chunk of the Agent's response"
([`prompt-turn.mdx` L17][pt-table]). An Agent **MAY** attach an opaque `messageId`. Chunks with
the same `messageId` belong to one message, and a changed one starts a new message
([`prompt-turn.mdx` L189–191][pt-ids]; [`schema.json` L3854][s-chunk]). The field is optional in
v1 and required in v2 ([`message-id.mdx` L119–120][rfd-msgid]). No v1 update marks a message
complete.

The v2 draft adds no end marker either. It adds `agent_message`, an upsert keyed by `messageId`
whose `content` replaces everything accumulated so far, including content that came from earlier
chunks ([`v2/message-updates.mdx` L11, L44–52, L69–93][rfd-v2-msg]). Under v2, a message can be
rewritten after it looks finished.

### Tool calls

- An Agent **SHOULD** report a tool call when the model requests it
  ([`tool-calls.mdx` L14][tc-create]). Its status defaults to `pending`
  ([L72–74][tc-create-status]).
- v1 has exactly four statuses: `pending` (input still streaming, or awaiting approval),
  `in_progress`, `completed` and `failed` ([`tool-calls.mdx` L235–252][tc-status];
  [`schema.json` L520][s-status]). v1 has no `cancelled` status. The v2 draft adds one
  ([`v2/tool-calls.mdx` L369–370][v2-tc-cancelled]).
- A `tool_call_update` is a patch. Every field except `toolCallId` is optional, and only the
  fields that changed need to be sent ([`tool-calls.mdx` L128–131][tc-update]). `content` and
  `locations` replace the whole collection ([`schema.json` L427][s-replace]). v1 therefore cannot
  append output to a tool call's `content`. The v2 draft adds `tool_call_content_chunk` for
  appending ([`v2/tool-call-updates.mdx` L11–13][rfd-v2-tc]).
- Once permission is granted, the Agent **SHOULD** mark the call `in_progress`. While it runs,
  the Agent **MAY** send more updates "providing real-time feedback about tool execution progress"
  ([`prompt-turn.mdx` L257, L274][pt-progress]). In v1, live output exists only as `terminal`
  content that points at a terminal the Client created with `terminal/create`
  ([`tool-calls.mdx` L297–312][tc-terminal]). kestrel's default capabilities offer no terminal.
- "If there are no pending tool calls, the turn ends and the Agent **MUST** respond"
  ([`prompt-turn.mdx` L239][pt-complete]). This is the closest the specification comes to
  requiring terminal statuses. No rule requires that a `tool_call` be followed by a terminal
  `tool_call_update`.

### Plans

A `plan` carries `entries`. Each update **MUST** contain the complete list, and the Client
**MUST** replace its current plan with it ([`agent-plan.mdx` L77–79][ap-replace];
[`schema.json` L4026][s-plan]). An Agent may add, remove or change entries as it works
([L83][ap-dynamic]). An entry is `pending`, `in_progress` or `completed`, and it has a required
priority ([L57–73][ap-entry]). Entries carry no ID, so an entry keeps no identity from one
snapshot to the next. The plan has no ID either, and no snapshot is marked final. The v2 draft
replaces `plan` with `plan_update`. That update carries a plan ID and a `cancelled` entry status
([`v2/plan-variants.mdx` L11–25][rfd-v2-plan]; [`v2/agent-plan.mdx` L98, L108][v2-ap]).

### Thoughts

`agent_thought_chunk` is "A streamed chunk of the Agent's reasoning"
([`prompt-turn.mdx` L18][pt-table]). It has the same shape as a message chunk and follows the
same `messageId` rule ([`schema.json` L3841–3854][s-chunk]). Nothing else bounds it. A thought
ends where a message ends: when the `messageId` changes, or when the turn ends.

### Turn end, cancellation and failure

- A turn ends with one of five `StopReason`s: `end_turn`, `max_tokens`, `max_turn_requests`,
  `refusal` or `cancelled` ([`prompt-turn.mdx` L314–332][pt-stop]).
- After sending `session/cancel`, the Client:
  - **SHOULD** mark every unfinished tool call as `cancelled`, a status that v1 does not define
    ([L348][pt-cancel]);
  - **MUST** answer each pending permission request with the `cancelled` outcome;
  - **SHOULD** still accept tool call updates that arrive after the cancel
    ([L348–367][pt-cancel]).
- On `session/cancel`, the Agent:
  - **SHOULD** stop model requests and tool calls as soon as possible;
  - **MAY** send more updates, as long as all of them precede its response;
  - **MUST** respond with `cancelled` once every operation is aborted and every pending update
    is sent;
  - **MUST** turn abort exceptions into `cancelled` rather than returning an error
    ([L348–367][pt-cancel]).
- No `StopReason` means failure. The prompt-turn page mentions errors only once, to say that
  aborted operations "may propagate as an error response" and that Agents must convert them to
  `cancelled` ([L356–361][pt-cancel]). The v1 error page contains only "Documentation coming
  soon" ([`error.mdx` L6][error]). Each Agent decides how a failed turn ends.
- "Session updates are not limited to active prompt turns" ([`prompt-turn.mdx` L30][pt-table]).
  Nothing forbids updates after the response.

## opencode 2.0.14

### Messages

Each `session.text.delta` becomes an `agent_message_chunk` whose `messageId` is the opencode
assistant message ID ([`event.ts` L193–201][oc-text]). opencode mints a new assistant message ID
for each model step. It keeps the ID when it retries a step that produced no output, and mints a
fresh one to continue after an interrupted stream or a compaction
([`llm.ts` L205–207, L224, L281–293][oc-step-id]; [`step.ts` L185–188][oc-retry]). Every text
part of one step therefore shares one `messageId`, even when a thought falls between two parts.
opencode's own test expects exactly this ([`event-behavior.test.ts` L188–193][oc-test]).

opencode knows where text ends, but the ACP layer drops that knowledge. `session.text.delta` is
ephemeral, and `session.text.ended` is "the replayable full-value boundary", carrying the full
text ([`session-event.ts` L396–419][oc-schema-text]). `streamTurn` has no case for
`session.text.ended`, `session.reasoning.ended` or `session.tool.input.ended`
([`event.ts` L182–336][oc-loop]).

### Thoughts

Each `session.reasoning.delta` becomes an `agent_thought_chunk` whose `messageId` is
`<assistantMessageID>:reasoning:<ordinal>` ([`event.ts` L202–210][oc-reasoning]). Every
reasoning part gets its own ID, distinct from the ID of the step's text. The durable
`reasoning.ended` boundary exists too, and is dropped in the same way
([`session-event.ts` L435–458][oc-schema-reasoning]).

### Tool calls

1. `session.tool.input.started` becomes a `pending` `tool_call` with empty input. Input deltas
   are not forwarded ([`event.ts` L211–229][oc-tool-start]; [`tool.ts` L66–80][oc-pending]).
2. `session.tool.called` becomes an `in_progress` `tool_call_update` with the full input
   ([`event.ts` L230–246][oc-tool-called]; [`tool.ts` L82–98][oc-running]).
3. Each `session.tool.progress` becomes another `in_progress` update with the same fields.
   opencode stores the progress metadata but does not send it, and the update has no `content`
   ([`event.ts` L247–261][oc-tool-progress]; [`session-event.ts` L512–520][oc-schema-progress]). A
   long-running call reports only that it is still running. Its output does not stream.
4. `session.tool.success` becomes `completed`, with the full `content` (plus a `diff` for an edit)
   and `rawOutput.metadata`. `session.tool.failed` becomes `failed`, with the error appended to
   `content` ([`event.ts` L262–305][oc-tool-done]; [`tool.ts` L100–161][oc-done]).

opencode settles every tool call it started. When a step ends, a user declines a call, the turn
is interrupted or the provider fails, opencode publishes `session.tool.failed` (type `aborted`,
"Tool execution interrupted") for each call still open
([`step.ts` L192–216][oc-settle]; [`publish-llm-event.ts` L327–353][oc-fail-tool]). The open
calls of a cancelled turn therefore reach `failed` before the response.

Subagent (child session) updates are folded into the parent turn when the client lacks
opencode's child-session capability. They carry `_meta["opencode/child-session"]`, and their
`toolCallId` gets a `<child>:` prefix ([`event.ts` L118–124, L404–420][oc-child]). After the
response, child updates go only to that capability's extension method
([`event.ts` L366–372][oc-background]), so kestrel receives nothing after the response.

### Plans

opencode 2.0.14 never sends one. [`packages/cli/src/acp`][oc-acp] contains no `plan` update.

### Turn end

- `session/cancel`, like `$/cancel_request`, marks the turn cancelled and calls
  `session.interrupt` ([`service.ts` L140–147][oc-cancel], [L331–358][oc-usage]).
- A cancel or interruption returns `cancelled`. `finish: length` returns `max_tokens`, and a
  content filter returns `refusal`. Everything else returns `end_turn`. opencode never returns
  `max_turn_requests` ([`event.ts` L588–598][oc-stop]).
- A failed execution, or an assistant error other than an abort or a content filter, throws.
  The client receives a JSON-RPC internal error carrying the error message, or `auth_required`
  for a provider authentication failure ([`event.ts` L327–336][oc-fail], [L557–572][oc-stop];
  [`error.ts` L74–85][oc-error]). A lost event stream also throws ([`event.ts` L129][oc-lost]).
- opencode sends one `usage_update` per turn: after the turn succeeds and before the response
  ([`service.ts` L347–350][oc-usage]).

## Claude Code through claude-agent-acp 0.79.0

### Messages

The `messageId` is the Anthropic API message ID. The adapter captures it at `message_start` and
applies it to every streamed chunk until the next `message_start`
([`acp-agent.ts` L5385–5390, L5496–5510][cl-msgid]; [L9215–9231][cl-grouping]). All text in one
model response shares one `messageId`, including text before and after the tool uses in that
response. Once streaming ends, the adapter compares the SDK's consolidated message with what it
already streamed and forwards only the remainder ([L5405–5411][cl-diff]). When nothing streamed
(a turn replayed from cache, or a local command), the adapter forwards the `result` text as a
single chunk with no `messageId` ([L5213–5250][cl-fallback]).

### Thoughts

`thinking` and `thinking_delta` become `agent_thought_chunk`s that carry the same `messageId` as
the text of their API message ([L9581–9595][cl-thinking]; [L9238–9250][cl-apply-id]). In this
runtime, a thought ends only where the update kind or the `messageId` changes. A model whose
`thinking.display` defaults to omitted streams thinking blocks with empty text, and these produce
no thought chunks ([L9582–9585][cl-thinking]).

### Tool calls

1. A `pending` `tool_call` is sent at `content_block_start`. It carries `name`, and for Bash it
   carries `terminal_info` only when the client declares `terminal_output`
   ([L9402–9432, L9938–9973][cl-tool-call]).
2. While the input streams, `tool_call_update` refinements carry the title, kind and locations,
   never content ([L9434–9466, L9974–10017][cl-refine]). The consolidated message refines the call
   once more ([L9679–9689][cl-refine-consolidated]).
3. The only source of `in_progress` is the SDK's `tool_progress` heartbeat. That update carries
   `elapsedTimeSeconds` in `_meta` and no content ([L5898–5955][cl-progress]).
4. A PostToolUse hook sends a `tool_call_update` carrying `_meta.claudeCode.toolResponse`, plus
   the diff for an Edit or Write ([L9622–9658][cl-hook]).
5. `tool_result` becomes `completed`, or `failed` when `is_error` is set, with the result as
   content ([L9819–9866][cl-result]). Output arrives whole at this point. Only a client that
   declares `terminal_output` receives it split into `_meta.terminal_output` and `terminal_exit`
   updates ([L9827–9846][cl-result]).

Two cases fall outside that sequence. A backgrounded Bash call reaches `completed` while its
command keeps running. The only sign of this is a `_meta` marker, sent only to clients that
declare async tasks ([L9307–9345][cl-background-bash]). TodoWrite and Task* calls never appear as
tool calls at all; they become `plan` updates ([L9252–9272][cl-plan-lane]).

### Plans

- TodoWrite: whenever the adapter sees a TodoWrite call whose input contains a `todos` array, it
  sends the whole list as a plan ([L9601–9608][cl-todowrite]).
- Task* (TaskCreate, TaskUpdate, TaskList): each successful call is applied to a task state kept
  for the ACP session, and the full snapshot is sent at `tool_result`
  ([L9774–9818][cl-task-plan]).

Every entry has priority `medium`. An `in_progress` entry shows its `activeForm` instead of its
content ([`tools.ts` L1126–1132, L1377–1383][cl-entries]), so an entry's text changes between
snapshots. The task state outlives the turn. A conversation reset clears it and sends an empty
plan ([`acp-agent.ts` L5971–5982, L2925–2933][cl-reset]).

### Turn end

- On `session/cancel`, the adapter sets a `cancelled` flag and calls `query.interrupt()`. It also
  arms a backstop that settles the turn `cancelled` if the SDK does not yield
  ([L6095–6101, L6298–6324][cl-cancel]).
- While that flag is set, the adapter drops every later SDK `assistant` and `user` message. Those
  messages carry the `tool_result`s of the interrupted calls ([L5620–5622][cl-guard]). The flag
  clears only when the next turn activates ([L3363–3373][cl-activate]). A tool call open at cancel
  therefore gets no terminal update from the adapter. Only the specification's suggestion that
  the Client mark unfinished calls `cancelled` would close it.
- The adapter returns `end_turn`, `max_tokens`, `refusal`, `cancelled`, and `max_turn_requests`.
  `max_turn_requests` covers turn, budget and structured-output limits that the SDK does not flag
  as errors ([L5139–5155, L5191–5317][cl-stop]).
- The prompt is rejected with a JSON-RPC error (internal error, or `auth_required`) on an error
  result, on a `/login` result, or when the query dies. A dead process also evicts the ACP session
  ([L3697–3717][cl-fail]; [L5191–5317][cl-stop]; [L6004–6064][cl-dead]).
- The turn settles at the SDK's `result`. The adapter keeps forwarding background output after the
  response ([L5318–5322][cl-drain]).
- The adapter sends a `usage_update` whenever the running token total changes at `message_start`
  or `message_delta`, and again on rate-limit events ([L5438–5494, L5957–5969][cl-usage]).

## Codex through codex-acp 1.12.0

### Messages

`item/agentMessage/delta` becomes an `agent_message_chunk`. Its `messageId` is the
`agentMessage` item's ID, and it carries `_meta.codex.phase` (for example `final_answer`)
([`CodexEventHandler.ts` L479–481, L657–660][cx-text]; [`ContentChunks.ts` L6–11][cx-phase]). The
adapter sends nothing for an `agentMessage`'s `item/started` or `item/completed`
([`CodexEventHandler.ts` L778–780, L841–843][cx-agent-item]). Codex reports where a message ends;
the adapter does not forward it.

Not every `agent_message_chunk` is text from the model. Config warnings, warnings, "*Context
compacted…*", review-mode output and turn errors all arrive as `agent_message_chunk`s with no
`messageId` ([L662–687, L952–962, L1149][cx-notices]). A plan-mode markdown plan arrives as one
`agent_message_chunk` (phase `final_answer`, `messageId` set to the plan item's ID) when the
client does not support plan updates ([L878–897, L944–950][cx-plan-text]).

### Thoughts

Reasoning deltas, whether summary or raw, become `agent_thought_chunk`s whose `messageId` is the
reasoning item's ID. A new summary part adds a `"\n\n"` chunk under the same ID
([L574–582, L721–745][cx-reasoning]). If no deltas streamed, the whole reasoning goes out as one
chunk at `item/completed` ([L832–836, L869–876][cx-reasoning-complete]). codex-acp turns reasoning
summaries off for API-key accounts and for models without reasoning
([`CodexAcpServer.ts` L2936–2939][cx-no-summary]). A "Model rerouted…" notice arrives as a
thought chunk with no `messageId` ([`CodexEventHandler.ts` L689–691][cx-rerouted]).

### Tool calls

1. The adapter creates the `tool_call` at `item/started`, mapping Codex's item status. A command
   therefore usually starts at `in_progress`, not `pending`
   ([L751–795][cx-item-start]; [`CodexToolCallMapper.ts` L55–66][cx-status]).
2. Command output streams. Each `item/commandExecution/outputDelta` becomes a `tool_call_update`
   with only `_meta.terminal_output_delta` and no standard `content`. That is the mode a client
   gets when it does not declare `terminal_output`
   ([`CodexEventHandler.ts` L548–550, L964–981][cx-output]; [`TerminalOutputMode.ts` L5–36][cx-mode]).
   MCP progress arrives as `_meta.mcp_output_delta` ([L999–1010][cx-mcp]).
3. `item/completed` becomes `completed` or `failed`. Codex's `failed`, `declined` and
   `interrupted` statuses all map to `failed`
   ([L797–821][cx-item-done]; [`CodexToolCallMapper.ts` L55–66][cx-status]). A completed command
   carries `rawOutput.formatted_output` and `exit_code` ([L1046–1080][cx-command-done]).

The adapter maps an `interrupted` item status. This note did not verify that the App Server
reports every in-flight item when a turn is interrupted.

### Plans

`turn/plan/updated` becomes a `plan` with the whole step list, and every entry has priority
`medium` ([L491–493, L1082–1093][cx-plan]).

### Turn end

- `session/cancel` sends `turn/interrupt`, and the interrupted `turn/completed` returns `cancelled`
  ([`CodexAcpServer.ts` L3348–3357, L3009–3012][cx-cancel]).
- The adapter returns only `end_turn` and `cancelled`
  ([L3009–3012, L3154–3158, L3265–3271][cx-stop]).
- A client without the typed-failure capability receives a terminal turn error as an
  `agent_message_chunk` of its text ([`CodexEventHandler.ts` L1134–1149][cx-error-text]). Only
  usage-limit and authentication errors become JSON-RPC errors
  ([L1140–1148][cx-error-text]; [`CodexAcpServer.ts` L3014–3018][cx-throw]). Any other failed turn
  returns `end_turn` ([L3019–3027, L3154–3158][cx-end]). A failed Codex turn can therefore look
  like a successful one whose last message is an error.
- After the prompt returns, the App Server subscription stays open for the life of the ACP
  session. Notifications that arrive late go through a session-scoped handler
  ([`CodexEventHandler.ts` L296–324][cx-late]).

## What kestrel does with these today

- `Heard::update` keeps two kinds of update. Text `AgentMessageChunk`s are joined into one string
  per run of the same `messageId`, and `UsageUpdate` is kept. `AgentThoughtChunk`, `Plan`,
  `ToolCall` and `ToolCallUpdate` only record that the turn produced something
  ([`runtime.rs` L724–765][ks-heard]).
- A turn that stops for any reason other than `end_turn` ends the conversation
  ([L691–701][ks-stop]).
- kestrel declares no client capabilities ([L149][ks-init], [L417][ks-init2]), and the supervisor never sends
  `session/cancel`.

## Left for the completed-unit decision

These are the facts the rule in
[Decide what a completed unit is for each thing a runtime emits](https://github.com/openkestrel/kestrel/issues/309)
has to absorb. They are stated here without a choice:

- A message ends only visibly: at a `messageId` change, or at the prompt response. The three
  runtimes cut `messageId`s at different grains: a model step, an API message, a Codex item.
- Two adapters send text the model did not write as `agent_message_chunk`s with no `messageId`:
  Codex's notices and errors, and Claude's `result` fallback.
- A tool call's terminal status is the only per-unit completion signal ACP has. Claude leaves
  calls open after a cancel, and whether Codex closes them on interrupt is unverified.
- Tool output streams only from Codex, and only in `_meta`. The other two runtimes deliver output
  whole, at completion.
- A plan has no final snapshot and its entries have no identity. opencode never sends a plan.
- The v2 draft makes messages rewritable after the fact, adds `cancelled` statuses for tool calls
  and plan entries, and adds appendable tool-call content.

Not established here: how the Codex App Server reports in-flight items on interrupt, and how
often the Claude SDK emits `tool_progress`. A live capture from each runtime would settle both.

[acp]: https://github.com/agentclientprotocol/agent-client-protocol/tree/322cb3767498fc75bbb4ebc9b57ef2b6ec958898
[pt-table]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/prompt-turn.mdx#L10-L30
[pt-ids]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/prompt-turn.mdx#L189-L191
[pt-complete]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/prompt-turn.mdx#L237-L251
[pt-progress]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/prompt-turn.mdx#L253-L276
[pt-stop]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/prompt-turn.mdx#L314-L332
[pt-cancel]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/prompt-turn.mdx#L334-L367
[tc-create]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/tool-calls.mdx#L12-L32
[tc-create-status]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/tool-calls.mdx#L72-L74
[tc-update]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/tool-calls.mdx#L98-L131
[tc-status]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/tool-calls.mdx#L235-L252
[tc-terminal]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/tool-calls.mdx#L297-L312
[ap-entry]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/agent-plan.mdx#L49-L73
[ap-replace]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/agent-plan.mdx#L75-L79
[ap-dynamic]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/agent-plan.mdx#L81-L83
[error]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v1/error.mdx#L6
[s-replace]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/schema/v1/schema.json#L380-L427
[s-status]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/schema/v1/schema.json#L520-L544
[s-chunk]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/schema/v1/schema.json#L3841-L3854
[s-plan]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/schema/v1/schema.json#L4021-L4026
[rfd-msgid]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/rfds/message-id.mdx#L119-L120
[rfd-v2-msg]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/rfds/v2/message-updates.mdx#L11-L93
[rfd-v2-tc]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/rfds/v2/tool-call-updates.mdx#L11-L13
[rfd-v2-plan]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/rfds/v2/plan-variants.mdx#L11-L25
[v2-tc-cancelled]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v2/tool-calls.mdx#L369-L370
[v2-ap]: https://github.com/agentclientprotocol/agent-client-protocol/blob/322cb3767498fc75bbb4ebc9b57ef2b6ec958898/docs/protocol/v2/agent-plan.mdx#L98-L108
[oc]: https://github.com/anomalyco/opencode/tree/v2.0.14
[oc-acp]: https://github.com/anomalyco/opencode/tree/v2.0.14/packages/cli/src/acp
[oc-child]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L118-L124
[oc-lost]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L129
[oc-loop]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L182-L336
[oc-text]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L193-L201
[oc-reasoning]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L202-L210
[oc-tool-start]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L211-L229
[oc-tool-called]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L230-L246
[oc-tool-progress]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L247-L261
[oc-tool-done]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L262-L305
[oc-fail]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L327-L336
[oc-background]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L366-L372
[oc-stop]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/event.ts#L557-L598
[oc-pending]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/tool.ts#L66-L80
[oc-running]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/tool.ts#L82-L98
[oc-done]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/tool.ts#L100-L161
[oc-cancel]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/service.ts#L140-L147
[oc-usage]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/service.ts#L315-L360
[oc-error]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/src/acp/error.ts#L74-L85
[oc-schema-text]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/schema/src/session-event.ts#L396-L419
[oc-schema-reasoning]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/schema/src/session-event.ts#L435-L458
[oc-schema-progress]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/schema/src/session-event.ts#L512-L559
[oc-step-id]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/core/src/session/runner/llm.ts#L205-L293
[oc-retry]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/core/src/session/runner/step.ts#L185-L188
[oc-settle]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/core/src/session/runner/step.ts#L192-L216
[oc-fail-tool]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/core/src/session/runner/publish-llm-event.ts#L327-L353
[oc-test]: https://github.com/anomalyco/opencode/blob/v2.0.14/packages/cli/test/acp/event-behavior.test.ts#L94-L195
[cl]: https://github.com/agentclientprotocol/claude-agent-acp/tree/v0.79.0
[cl-plan-lane]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9252-L9272
[cl-grouping]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9215-L9231
[cl-apply-id]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9238-L9250
[cl-background-bash]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9307-L9345
[cl-tool-call]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9402-L9432
[cl-refine]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9434-L9466
[cl-thinking]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9581-L9595
[cl-todowrite]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9601-L9608
[cl-hook]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9622-L9658
[cl-refine-consolidated]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9669-L9694
[cl-task-plan]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9774-L9818
[cl-result]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L9819-L9866
[cl-msgid]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L5385-L5390
[cl-diff]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L5405-L5411
[cl-usage]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L5438-L5494
[cl-fallback]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L5213-L5250
[cl-stop]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L5139-L5317
[cl-drain]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L5318-L5322
[cl-guard]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L5620-L5622
[cl-progress]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L5898-L5955
[cl-reset]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L5971-L5982
[cl-dead]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L6004-L6064
[cl-cancel]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L6095-L6324
[cl-activate]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L3363-L3373
[cl-fail]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts#L3697-L3717
[cl-entries]: https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/tools.ts#L1126-L1132
[cx]: https://github.com/agentclientprotocol/codex-acp/tree/v1.12.0
[cx-late]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L296-L324
[cx-text]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L657-L660
[cx-plan]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L1082-L1093
[cx-output]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L964-L981
[cx-reasoning]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L721-L745
[cx-notices]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L662-L687
[cx-rerouted]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L689-L691
[cx-item-start]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L751-L795
[cx-agent-item]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L778-L780
[cx-item-done]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L797-L821
[cx-reasoning-complete]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L832-L876
[cx-plan-text]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L878-L950
[cx-mcp]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L999-L1010
[cx-command-done]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L1046-L1080
[cx-error-text]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexEventHandler.ts#L1134-L1149
[cx-phase]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/ContentChunks.ts#L6-L11
[cx-mode]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/TerminalOutputMode.ts#L5-L36
[cx-status]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexToolCallMapper.ts#L55-L66
[cx-no-summary]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexAcpServer.ts#L2936-L2939
[cx-cancel]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexAcpServer.ts#L3348-L3357
[cx-stop]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexAcpServer.ts#L3009-L3012
[cx-throw]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexAcpServer.ts#L3014-L3018
[cx-end]: https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/CodexAcpServer.ts#L3019-L3158
[ks-init]: https://github.com/openkestrel/kestrel/blob/f8e56594491a0f046528d7a3ad4b9d544d5460a0/crates/kestrel-supervisor/src/runtime.rs#L149
[ks-init2]: https://github.com/openkestrel/kestrel/blob/f8e56594491a0f046528d7a3ad4b9d544d5460a0/crates/kestrel-supervisor/src/runtime.rs#L417
[ks-stop]: https://github.com/openkestrel/kestrel/blob/f8e56594491a0f046528d7a3ad4b9d544d5460a0/crates/kestrel-supervisor/src/runtime.rs#L691-L701
[ks-heard]: https://github.com/openkestrel/kestrel/blob/f8e56594491a0f046528d7a3ad4b9d544d5460a0/crates/kestrel-supervisor/src/runtime.rs#L724-L765
