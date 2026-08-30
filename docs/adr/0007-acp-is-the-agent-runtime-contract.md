# ACP is the agent-runtime contract, and kestrel is one language

kestrel drives an agent runtime by speaking the [Agent Client Protocol](https://agentclientprotocol.com)
as a client, sourced throughout in [`docs/research/acp.md`](../research/acp.md) on branch
`research/acp`. There is no in-house driver interface: a runtime is ACP-speaking or it is
unsupported. This supersedes
[ADR-0003](0003-agent-runtime-contract-is-an-in-process-interface.md), and it removes the only reason
[ADR-0001](0001-rust-control-plane-typescript-supervisor.md) gave for a second language, so the
supervisor becomes Rust and TypeScript leaves kestrel's source.

**ADR-0001's load-bearing claim is false.** It holds that the Claude Agent SDK's `canUseTool` is *"the
only surface that carries that round-trip"* — the mid-run approval a [`CONTEXT.md`](../../CONTEXT.md)
**Approval** requires. ACP's `session/request_permission` is a blocking JSON-RPC request/response
offering `allow_once | allow_always | reject_once | reject_always`, and every ACP agent has it. The
Rust and TypeScript SDKs both reached 1.0 on 2026-06-25; the `agent-client-protocol` crate implements
the `Client` trait, which is the side kestrel is on.

**ADR-0003's objection does not survive the inversion.** It rejects "the wire protocol is the
contract" because a second author would have to satisfy the lease, the cursor semantics, the
at-least-once replay and the `ask` round-trip — *"heavy contracts do not get second implementations."*
That is an argument about a contract kestrel **publishes**. ACP is one kestrel **consumes**, and it
already has roughly thirty-five implementations, including opencode natively (`opencode acp`), with
Codex CLI and Claude Code reached through Zed's adapters. Nobody has to implement anything for
kestrel; kestrel implements someone else's client.

**The language follows the contract, and is not a decision of its own.** The supervisor is a second
Rust binary in the same Cargo workspace as `kestrel`, not a fourth argv role: `kestrel-env` is pulled
once per environment and runs adjacent to untrusted agent output, so it should not carry the store,
the six ports, the CLI, or the migrations. TypeScript does not vanish from the world — Zed's
`claude-code-acp` is Node — but it stops being kestrel's source, kestrel's CI, and kestrel's reason
for a language. The default image no longer contains Node at all, because the `claude` binary leaves
it: opencode remains the default and speaks ACP with nothing in the path.

## What this costs

Three properties are traded away on purpose. They are listed because each one was previously written
down as a promise.

**Supervisor death becomes run death.** ACP's stable transport is JSON-RPC over stdio, so the agent
is the supervisor's child process. ADR-0002's consequence *"Supervisor death is not run death as long
as the runtime process lives"* no longer holds, and the issue that tested it is closed rather than
deferred. Nothing replaces it: the run-held lease expires and the run ends with an explicit exit
status, which is what `0.1` already promises for an interrupted run, and *kestrel retries dispatch,
never work*. ADR-0002's own `0.1` test — `kill -9` the **control plane**, restart, gap-free
transcript — is untouched, because the supervisor stays alive across it holding its `Last-Event-ID`.

The neighbouring loss is that ACP has no sequence cursor to reconnect with even if it could: v1's
`session/resume` restores without replay, v2 folds `session/load` into it behind a `replayFrom` whose
only defined cursor is `{"type": "start"}` — replay everything — and durability across an agent
restart is explicitly left to the implementation. opencode's `GET /api/session/:id/event?after=<seq>`
has no counterpart in the protocol and is not coming.

**Tool-input rewriting leaves the contract.** ADR-0003 reserved a per-tool-call decision point from
`0.1`, unused, on the grounds that it was cheap then and expensive to retrofit. ACP cannot fill it:
permission is option-selection only in both versions, and v2 says so outright — *"The command
describes what the Agent wants permission to execute and never asks the Client to execute it."* The
decision point survives in a better shape than the one it replaces, as v2's permission `subject`
tagged union: a `command` variant carrying `command` and an absolute `cwd` governs a shell invocation
more directly than rewriting a tool's arguments ever did, and a subjectless variant carries a generic
**Approval**. If kestrel ever must narrow an operation rather than judge it, it does so by hosting an
MCP server the agent is configured with — never as a capability of the contract.

**The v1 gate gets materially cheaper.** [`README.md`](../../README.md)'s gate — the contract proven
against a second runtime — is meaningless against a contract kestrel does not own, since two ACP
agents prove ACP works rather than that kestrel's seam absorbs churn. The replacement is two agents
of different lineages, one native and one adapter-wrapped, which does test the thing that actually
breaks: resume behaviour, permission granularity, and declared capabilities all differ across that
line, and a client with opencode-shaped assumptions fails on it. This is honest work and it is a
day's work where the old gate was a month's. The lost signal is recorded here rather than argued
away. The rule of two loses a member with it: the agent runtime is no longer a kestrel-pluggable
layer, so `Compute` and `Store` carry that rule at v1.

## Consequences

**Wire v1, shape v2.** v1 is stable and is what `opencode acp`, `codex-acp` and `claude-code-acp`
ship against. v2 was published as a draft on 2026-07-20 and says it may change incompatibly in any
SDK release; kestrel's v1 is a stability lock and cannot freeze on someone else's draft. So kestrel
speaks v1 on the wire while modelling permissions as v2's `subject` union — with `tool_call` the only
populated variant under v1 — and resume as v2's unified `session/resume`. v2 then arrives as a wire
change rather than a domain change. Gating kestrel's v1 on v2 stabilizing was rejected: it hands the
release date to Zed and JetBrains, who govern ACP jointly under two lead maintainers with explicit
veto and no published backwards-compatibility commitment.

**Credentials reach the agent through the spawn, not the protocol.** ACP will not carry them — *"The
descriptor cannot provide a command. The Client derives the command from its own Agent
configuration."* The supervisor spawns the agent with the provider key in its process environment,
received over the link for that run only and never written to disk, which satisfies capability #4's
*"reaching an environment only when a run needs them"* with no protocol feature at all. ACP's
`authenticate` covers subscription logins instead, and its `terminal` auth type launches an
interactive process: an agent offering only that cannot be driven headlessly and fails at
`initialize` rather than hanging.

**A run that cannot have the model it was configured with fails.** Config options are optional and
every agent ships a default, so a runtime may expose no `model` category to
`session/set_config_option`. When an **Agent** names a model and the runtime cannot honour it, the run
fails at `initialize`; when an **Agent** names none, the runtime's default is the honest answer.
Running something other than what was configured would leave an audit record that lies, which is the
worst of the three available outcomes.

**A `Run` owns exactly one ACP session.** Environments are disposable per run and an agent cannot
outlive its parent supervisor, so ACP's `sessionId` is a run's identifier at the runtime rather than
a concept, and [`CONTEXT.md`](../../CONTEXT.md) gains no term for it.

**`session/update` is filtered, not forwarded.** `agent_message_chunk` is coalesced by `messageId`
into one transcript entry at turn end; `plan`, `tool_call`, `tool_call_update` and thought chunks
reach no transcript. `usage_update` carries the token and cost figures a campaign spend cap needs at
`0.5`, so it is recorded on the **Run** — not in the transcript, and not discovered after the run
shape is frozen.

**The `kestrel-env` image bundles opencode and nothing else.** Installing an arbitrary agent from the
ACP Registry lands at `0.7`, under two constraints that are cheap now and awkward later: kestrel pins
and records the resolved digest on the **Run**, because an audit record that cannot identify what
executed is worse than an absent feature; and resolution happens at image build or into a warm cache,
never during provisioning, so no run fails because a CDN blinked. The registry itself offers binary,
`npx` and `uvx` distribution with no checksums, no signatures and no pinning — its CI validates only
that the URLs resolve — and it is curated to agents supporting authentication, which selects for
interactive editor users rather than headless runners.
