# The Agent Client Protocol as kestrel's agent-runtime seam

Backs [ADR-0007](../adr/0007-acp-is-the-agent-runtime-contract.md). Sources are ACP's own
documentation (<https://agentclientprotocol.com>), read 2026-08-30. Every claim below cites the page
that owns it. ACP **v1 is stable and v2 is a draft**; the v2 sections are the ones most likely to
have moved by the time you read this.

This audit exists because [ADR-0001](../adr/0001-rust-control-plane-typescript-supervisor.md) made a
falsifiable claim, and it turned out to be false.

## Headline: the claim that put TypeScript in this repository is wrong

ADR-0001 holds that the Claude Agent SDK's `canUseTool` is *"the only surface that carries that
round-trip"* — a mid-run approval that blocks until a human resolves it, which is what
[`CONTEXT.md`](../../CONTEXT.md) defines an **Approval** to be.

**ACP carries it, and every ACP agent has it.** The Agent sends `session/request_permission` to the
Client with `sessionId`, `toolCall` and an `options` array; the Client replies
`{"outcome": {"outcome": "selected", "optionId": "..."}}` or `{"outcome": "cancelled"}`. Option kinds
are `allow_once`, `allow_always`, `reject_once`, `reject_always`.
([tool-calls](https://agentclientprotocol.com/protocol/v1/tool-calls))

The reason ADR-0001 gave for a second language therefore does not exist. It was a reasonable reading
of Anthropic's surface in isolation; it was wrong about the field.

**The Rust SDK is real and is the side kestrel needs.** The `agent-client-protocol` crate exposes
both an `Agent` trait and a `Client` trait — kestrel implements `Client`.
([libraries/rust](https://agentclientprotocol.com/libraries/rust)) The Rust and TypeScript SDKs both
reached 1.0 on 2026-06-25.
([sdk-1-0-releases](https://agentclientprotocol.com/announcements/sdk-1-0-releases))

## What ACP is

JSON-RPC 2.0 over stdio for local agents run as subprocesses; HTTP and WebSocket exist for remote
agents but are called work-in-progress.
([introduction](https://agentclientprotocol.com/get-started/introduction),
[transports](https://agentclientprotocol.com/protocol/v1/transports)) A Streamable-HTTP/WebSocket RFD
and a transports working group are open, so treat stdio as the only transport you can build on today.
([rfds/streamable-http-websocket-transport](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport))

The **Client** is the editor or IDE; the **Agent** is the coding agent. kestrel is a Client that
happens not to be an editor, which is the one place its use is off the beaten path.

## The field is already wide

Roughly thirty-five agents implement it natively — Cursor, GitHub Copilot (public preview since
January 2026), Goose, Cline, OpenHands, Junie, Qwen Code, Kiro CLI, Docker's cagent, Mistral Vibe and
others — with **Codex CLI, Gemini CLI and Claude Code reached through adapters**, Zed's in the case of
the first and third. ([agents](https://agentclientprotocol.com/get-started/agents))

**opencode speaks it natively.** `opencode acp` starts it as an ACP subprocess over JSON-RPC on
stdio, carrying its built-in tools, custom tools and slash commands, MCP servers, `AGENTS.md` rules,
and its agents and permissions system. `/undo` and `/redo` are documented as unsupported.
([opencode.ai/docs/acp](https://opencode.ai/docs/acp/))

This is the fact that inverts ADR-0001 completely: the default runtime needs **no** adapter and no
Node, while Claude — the runtime that justified the whole TypeScript branch — is the one that needs
both.

## Where ACP is weaker than what kestrel already had

### There is no sequence cursor, and none is coming

[ADR-0002](../adr/0002-two-deployables-the-environment-dials-out.md) leans on opencode's
`GET /api/session/:id/event?after=<seq>` durable replay. ACP has no counterpart.

- **v1**: `session/resume` restores conversation state *without* returning prior messages,
  distinguishing it from `session/load`. The capability is declared as
  `{ sessionCapabilities: { resume: {} } }`, and agents decide for themselves whether to "replay
  events or do not." No cursors, no sequence numbers, no ordering guarantees.
  ([rfds/session-resume](https://agentclientprotocol.com/rfds/session-resume))
- **v2**: folds `session/load` and `session/resume` into one `session/resume` with an optional
  `replayFrom`. Its only defined cursor is `{"type": "start"}` — *"replay the whole conversation
  through `session/update` notifications before responding."* Cursors are inclusive, and
  message-identifying cursors are named only as a future possibility. Durability across an agent
  process restart is not specified.
  ([rfds/v2/session-resume-replay](https://agentclientprotocol.com/rfds/v2/session-resume-replay))

Combined with stdio parenting — the agent is the Client's subprocess — this is why ADR-0007 gives up
ADR-0002's *"Supervisor death is not Run death"* consequence rather than trying to preserve it.

### Permission is option-selection only, in both versions

The Client cannot rewrite a tool's input, add conditions, or execute anything. v2 states it directly:
*"The command describes what the Agent wants permission to execute and never asks the Client to
execute it."* ([rfds/v2/permission-requests](https://agentclientprotocol.com/rfds/v2/permission-requests))

This kills the input-rewriting half of
[ADR-0003](../adr/0003-agent-runtime-contract-is-an-in-process-interface.md)'s reserved decision
point. What v2 gives instead is arguably better suited to kestrel's **Policy**: permission requests
move from `toolCall`-specific to an optional `subject` tagged union with `tool_call`, `command` and
subjectless variants, a required `title` and optional `description`. The `command` variant carries
the command and an absolute `cwd`; the subjectless variant covers *"permission requests valid without
a structured target."*

### The protocol will not carry a credential

*"The descriptor cannot provide a command. The Client derives the command from its own Agent
configuration."* Agents advertise `authMethods` at `initialize`; the Client calls `authenticate` with
a method id for the default `agent` type, or launches an interactive process for the `terminal` type.
([authentication](https://agentclientprotocol.com/protocol/v1/authentication))

For kestrel this is a feature, not a gap: the Client spawning the agent means the provider key rides
in the subprocess environment for that Run and dies with it. The `terminal` type is the one to
refuse — there is no human at a terminal inside an **Environment**.

### Model selection is optional

Agents advertise a `configOptions` array at session setup, each with `id`, `name`, `type` (`select`,
`boolean`) and `currentValue`; the Client calls `session/set_config_option` with `sessionId`,
`configId` and `value`, and gets the complete list back. A `model` category is the semantic
classification for model selection, with `model_config` for related parameters. But support is not
mandatory, and *"Agents MUST always provide a default value for every configuration option."*
([session-config-options](https://agentclientprotocol.com/protocol/v1/session-config-options))

So a runtime may expose no way to pick a model at all, which is why ADR-0007 fails a Run whose
**Agent** named one rather than quietly running another.

## The event vocabulary kestrel must filter

`session/update` notification variants: `plan` (entries with content, priority, status),
`agent_message_chunk` (identified by `messageId`), `tool_call` (`toolCallId`, title, kind, status),
`tool_call_update` (status transitions and results), and `usage_update` (token usage, capacity,
optional cost). A prompt turn ends with a `StopReason`: `end_turn`, `max_tokens`,
`max_turn_requests`, `refusal`, or `cancelled`.
([prompt-turn](https://agentclientprotocol.com/protocol/v1/prompt-turn))

`usage_update` is the one to notice early — it is what a campaign spend cap needs at `0.5`, it is not
transcript material, and discovering it after the **Run** shape is frozen would be the expensive
order.

## The registry, and why it is not a `0.1` dependency

Clients discover agents from a curated index at
`https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`, maintained by pull request
against `agentclientprotocol/registry`. Entries carry name, description, version, homepage,
repository, icon and distribution metadata.
([registry](https://agentclientprotocol.com/get-started/registry))

Distribution is one of three forms: **binary** archives, which *"must be provided for all three
operating systems"* (darwin, linux, windows), **npx**, or **uvx**.
([rfds/acp-agent-registry](https://agentclientprotocol.com/rfds/acp-agent-registry))

**There are no checksums, no signatures and no pinning.** CI validates only that distribution URLs
resolve and archives are obtainable before publication. For an editor installing onto a developer's
laptop that bar is defensible; for kestrel, which installs into a governed **Environment** and then
tells an **Audit Record** what ran there, it is not — hence ADR-0007's requirement that kestrel pin
and record a digest itself.

Note also that inclusion requires supporting at least one authentication method, Agent Auth or
Terminal Auth. That curation selects for interactive editor users, which is not the axis a headless
runner cares about.

## Governance, and what it means for a project whose v1 is a stability lock

ACP is governed **jointly by Zed and JetBrains** — not a foundation, though the two are *"working
toward transitioning to an independent foundation."* Two lead maintainers, Ben Brandt (Zed) and
Sergey Ignatov (JetBrains), are described as BDFL with veto over all decisions; core maintainers meet
fortnightly to vote on proposals; smaller changes go through an RFD process on Zulip.
([governance](https://agentclientprotocol.com/community/governance))

**No backwards-compatibility commitment or version support window is published.** That is the
material risk kestrel takes on, and it is why ADR-0007 rejects gating kestrel's v1 on ACP v2
stabilizing: that would hand kestrel's release date to two other companies.

## v1 versus v2

v2 was published as a draft on 2026-07-20 and says its wire protocol and TypeScript API may change
incompatibly in any SDK release. It exists to express more session states, consolidate patterns and
improve consistency; one motivating change is that v1 did not prohibit `session/update` outside a
turn, and enforcing that would prevent an agent from reporting background events while ready for new
input. ([announcements/acp-v2-draft](https://agentclientprotocol.com/announcements/acp-v2-draft))

The migration guide recommends supporting **both** concurrently, since v1 peers will remain in the
wild well after v2 stabilizes, and since version negotiation yields one protocol version per
connection: keep two thin protocol surfaces behind shared application logic and select one after
initialization. ([protocol/v2/migration](https://agentclientprotocol.com/protocol/v2/migration))

That is exactly the shape ADR-0007 adopts, minus the v2 surface until it stabilizes: speak v1 on the
wire, model the domain on v2.

## What this supersedes

[`docs/research/opencode-sdk.md`](https://github.com/jtmthf/kestrel/blob/research/opencode-sdk/docs/research/opencode-sdk.md)
on branch `research/opencode-sdk` audited opencode's REST/SSE surface as the thing kestrel would
drive. Its findings still hold about opencode, but kestrel no longer drives that surface: it drives
`opencode acp`. Two of its conclusions survive the change intact and are worth carrying forward —
opencode's event stream is not a durable log and kestrel must own its own, and opencode offers no
session sharing kestrel could delegate multiplayer to. Neither of those was ever going to be solved
by a protocol change.
