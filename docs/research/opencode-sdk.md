# opencode 2 SDK audit

Resolves [#2](https://github.com/jtmthf/kestrel/issues/2). Sources are opencode's own v2 documentation
(<https://opencode.ai/v2/docs/>), read 2026-08-26. Every claim below cites the page that owns it.
opencode 2 is in **beta**; anything here can change, and the two surfaces kestrel depends on most are
explicitly named as still changing.

## Headline: the multiplayer thesis cannot be delegated to opencode

**opencode has no working session sharing in v2.** The sharing page states plainly that
"Session sharing is not yet available in OpenCode V2." The `share` config field (`manual` / `auto` /
`disabled`) is "parsed but ... not acted on by the current V2 runtime," and there is "no public session
viewer, share URL, history sync, retention controls, or unshare/delete operation." The docs advise not to
"treat the `share` configuration field as a privacy or publishing control."
([sharing](https://opencode.ai/v2/docs/sharing/))

Where sharing is described at all, it implies a hosted "sharing service" rather than a self-hostable
component — which would have been a poor fit for kestrel regardless.

**But multi-client access is mechanically available at the HTTP layer.** The server exposes a plain REST
API: `POST /api/session` creates a session, `POST /api/session/{sessionID}/prompt` sends it a prompt, and
`GET /api/event` is a server-wide Server-Sent Events subscription. Nothing restricts those calls to one
caller. ([api](https://opencode.ai/v2/docs/api/))

So: **many clients can drive one opencode session, but opencode does not offer that as a feature and does
not manage it.** Fan-out, presence, turn-taking, conflict handling, and history are kestrel's to own.
That is a larger commitment than "opencode gives us sessions, we add users on top."

### The event stream is not a durable log

`GET /api/event` is "volatile by contract: a slow consumer overflows and fails the stream, and events
during disconnection are missed." ([api](https://opencode.ai/v2/docs/api/))

This is the sharpest constraint in the audit. A durable multiplayer session — one humans rejoin days later
— cannot be built directly on this stream. kestrel must persist its own event log and treat opencode's
stream as a lossy live feed, not a source of truth. There is a
`GET /api/experimental/session/{sessionID}/log` with live-follow, but it is marked experimental.

Also note `GET /api/session/active` returns "foreground Session drains currently owned by this OpenCode
process," which points at single-process session ownership — relevant to scheduling sessions across
machines. ([api](https://opencode.ai/v2/docs/api/))

## Two embedding modes

| | `@opencode-ai/client` | `@opencode-ai/sdk` |
|---|---|---|
| Shape | network client to a running server | assembles the server **in-process** |
| Transport | HTTP | "routes API calls through its HTTP router in memory ... opens no HTTP listener and adds no network hop" |
| Entry | `OpenCode.make({ baseUrl })` | `await using opencode = await OpenCode.create()` |

Both expose "the same Promise values, declared errors, request options, and `AsyncIterable` streams."
([build/client](https://opencode.ai/v2/docs/build/client), [build/sdk](https://opencode.ai/v2/docs/build/sdk))

```ts
const client = OpenCode.make({ baseUrl: "http://localhost:4096" })
const session = await client.session.create({ location: { directory: "/workspace" } })
await client.session.prompt({ sessionID: session.id, text: "..." })
for await (const event of client.event.subscribe()) { console.log(event.type) }
```

For Node, `@opencode-ai/client/service` manages the process lifecycle: `Service.discover()`,
`Service.ensure()` (accepts version predicates and custom commands), `Service.stop()`.
([build/client](https://opencode.ai/v2/docs/build/client))

**For kestrel the network client is the right seam**, not the embedded SDK: the agent runs inside an
isolated compute environment while the control plane sits outside it, so a process and network boundary
is wanted, not something to optimize away.

## Session model — and a vocabulary collision

An opencode session is created against a directory (`location: { directory: "/workspace" }`), carries an
ID, receives prompts, and can be listed. ([build/client](https://opencode.ai/v2/docs/build/client))

This **collides directly with kestrel's intended "session" noun**. kestrel means a long-lived,
multi-human, possibly multi-environment collaboration; opencode means one agent conversation bound to one
directory in one process. They are not the same thing and cannot share a word.
Flagged for [#7](https://github.com/jtmthf/kestrel/issues/7) — a kestrel session likely *contains* one or
more opencode sessions over its life.

## Persistence and resumption: not what kestrel needs

Snapshots are a **rollback/undo** feature, not session portability. They "let clients roll back
conversation history and related file changes," capturing filesystem state "immediately before the model
call and after a cleanly completed step," stored in "a separate internal Git object database in the
OpenCode data directory." Untracked files over 2 MiB and git-ignored files are excluded.
([snapshots](https://opencode.ai/v2/docs/snapshots/))

Nothing documents resuming a session on a **different machine**. Combined with the volatile event stream
and per-process session ownership, the conclusion is: **durable, portable sessions are kestrel's problem
to solve, not a capability it inherits.**

## Model routing: genuinely pluggable, with leaks

Providers are configured in `opencode.jsonc`, keyed by provider ID, each with `name`, `env`, `package`,
`settings`, `headers`, `body`, `models`. ~20+ native packages ship (OpenAI, Anthropic and
Anthropic-compatible, Google + Vertex, Azure, Bedrock, OpenRouter, XAI), and you can point `package` at
any npm package or a `file://` URL for a local one. ([providers](https://opencode.ai/v2/docs/providers/))

The abstraction is real, but **vendor specifics leak**: "`settings` is package-specific. A field only has
an effect when the selected package supports it." So "any model" is true at the routing layer while
per-provider configuration remains bespoke — kestrel should promise model *choice*, not uniform model
*behavior*.

**Undetermined:** whether the model can be selected per-session or per-request programmatically. The
provider docs show only config-level selection (`"model": "openai/coding"`), and the client/API pages I
read do not document a per-prompt model override. `GET /api/model` and `GET /api/model/default` exist
([api](https://opencode.ai/v2/docs/api/)), which implies runtime awareness, but I could not confirm a
per-request override from primary docs. Worth settling before kestrel promises per-job model choice.

## Governance: the plugin hooks are the seam

Permissions are **runtime-enforced, not advisory** — `deny` blocks execution, `allow` proceeds, `ask`
waits. V2 uses "the `permissions` field and an ordered array of rules."
([permissions](https://opencode.ai/v2/docs/permissions/))

```jsonc
{ "permissions": [
  { "action": "*", "resource": "*", "effect": "ask" },
  { "action": "read", "resource": "*", "effect": "allow" }
] }
```

Config is static, but the **plugin API is where an external supervisor plugs in**
([build/plugins](https://opencode.ai/v2/docs/build/plugins)):

- **Permission** — `evaluate`: reviews the decision *after* rule evaluation and may set `effect` to
  `allow` / `ask` / `deny`. This is the hook that lets kestrel enforce policy at the execution layer,
  which is precisely what the background-agents thesis demands.
- **Tool** — `execute.before` (inspect or replace tool input), `execute.after` (inspect `status`,
  `result`, `error`). Full tool-call visibility, i.e. the audit trail.
- **Shell** — `create.before`: modify command, cwd, timeout, executable, env.
- **Session** — `context`, `model.request`, `http.request`, `http.response`.

One trap: "Non-interactive clients must decide how to handle requests that require approval; explicit
`deny` rules remain enforced." **kestrel is a non-interactive client**, so every `ask` becomes kestrel's
to answer — which is either a governance feature (route to a human in the session) or a hang, depending
entirely on how kestrel handles it.

## Beta reality — the risk kestrel is accepting

> "OpenCode 2.0 is in beta. Features may break unintentionally, and the server and plugin APIs may
> continue to change." ([migrate-v1](https://opencode.ai/v2/docs/migrate-v1/))

No GA date or stability guarantee is stated. Intentional v1→v2 breaking changes: plugins use a new API
("V1 plugins will not work in V2"), server API and clients have new contracts, and CLI config consolidates
into `cli.json`. Users are told to "Keep your V1 setup until you have confirmed the V2 behavior you need."

**The two APIs named as still changing — server and plugin — are exactly the two kestrel's architecture
rests on.** Since kestrel's load-bearing non-goal is that it never grows its own agent loop, opencode's
stability is kestrel's stability. This belongs on the roadmap as a standing risk, not a footnote.

## What kestrel must own (the gaps)

1. **Multiplayer entirely** — fan-out, presence, turn-taking, conflict resolution. Not an opencode feature.
2. **A durable event log** — opencode's stream is explicitly lossy.
3. **Session portability across machines** — snapshots do not provide it.
4. **Answering `ask` permission prompts** as a non-interactive client.
5. **A different word than "session"**, or an explicit containment relationship.

## Open questions

- Per-request/per-session model override: documented anywhere? (blocks a clean "any model, per job" promise)
- Does `GET /api/experimental/session/{sessionID}/log` offer replay from an offset — i.e. could it back a durable log?
- Auth on the server API: `UnauthorizedError` (401) appears throughout but the credential mechanism was not documented on the pages read.
