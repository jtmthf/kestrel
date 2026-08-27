# Open-source landscape check — does the gap exist?

Resolves [#5](https://github.com/jtmthf/kestrel/issues/5). Read 2026-08-26. Framed as a falsification
exercise: the job was to find the project that makes kestrel redundant.

## Verdict: the thesis is false as written

**"No open-source entrant exists in this category" does not survive contact with the landscape.**
At least one MIT-licensed project occupies kestrel's described position almost exactly, and a second,
much larger one covers a big part of it. The front page cannot lead with "nothing open exists."

A narrower claim does survive, and it is still worth building on. It is in the last section.

## The direct hit: Open-Inspect / `background-agents`

[github.com/ColeMurray/background-agents](https://github.com/ColeMurray/background-agents) — an
open-source implementation of Ramp's internal Inspect, by Cole Murray.

Primary-source metadata via the GitHub API on 2026-08-26:

| | |
|---|---|
| License | **MIT** |
| Stars / forks | **2,684 / 393** |
| Created | 2026-01-25 |
| Last push | **2026-08-27** (actively maintained) |
| Open issues | 105 |
| Home | <https://backgroundagents.dev> |

What it does, from its README:

- **Triggers**: web UI, Slack messages and DMs, GitHub PR comments and @mentions, Linear issue mentions
  and assignments, authenticated HTTP webhooks, cron schedules, Sentry alerts.
- **Agent runtime**: **OpenCode** — the same choice kestrel has made.
- **Sandbox providers**: Modal, Daytona, Vercel Sandbox, OpenComputer, E2B — i.e. already pluggable.
- **Multiplayer**: "Multiple users collaborate in same session," with presence indicators and prompts
  "attributed to their authors in git commits."
- **Control plane**: Cloudflare Workers + Durable Objects (session state, WebSocket hub, event stream),
  D1 for repo-scoped secrets.
- **Data plane**: sandbox containing a supervisor, OpenCode, and a bridge back to the control plane.

Read against kestrel's charting decisions, this covers: multi-source ingest, isolated execution,
model choice, durable multiplayer sessions, and a control plane. That is most of the v1 capability set.

**This is the project the falsification exercise was looking for, and it exists.**

### Where it stops

Two limitations are stated by the project itself, and both are structural rather than incidental:

1. **It requires Cloudflare.** The control plane is Workers + Durable Objects + D1. *No local or
   Kubernetes deployment path is documented.*
2. **It is single-tenant by design.** The README describes it as "designed for single-tenant deployment
   only, where all users are trusted members of the same organization," advises deploying "behind your
   organization's SSO/VPN," and notes that shared GitHub App credentials mean **"any user of the system
   can access any repo the App has access to."** Multi-tenancy "would require architectural changes."

There is also a dormant fork, [`appsumo/openinspect`](https://github.com/appsumo/openinspect)
(0 stars, last push 2026-04-16) — not a separate competitor.

## The large adjacent: OpenHands

[OpenHands](https://github.com/OpenHands/OpenHands) — MIT, **85,200 stars**, 11,138 forks, last push
2026-08-27. Far and away the most established self-hostable agent harness, and it has moved toward
kestrel's territory during 2026.

**Agent Canvas** ([announcement](https://www.openhands.dev/blog/introducing-agent-canvas), 2026-06-16)
is "OpenHands' workspace for creating automations that integrate with Slack, GitHub, and the tools where
engineering work happens" — event-driven automations from Slack, GitHub, and Linear plus scheduled runs,
described as "becoming the main interface for OpenHands." Deployment is flexible: "Start locally on your
laptop, move it to a VM to keep your agents always-on," or use OpenHands Cloud/Enterprise.

**The important nuance — governance appears to be a commercial tier.** OpenHands' own comparison post
states: "OpenHands Enterprise adds the control layer for governed agent execution, including sandboxed
runtimes, access controls, auditability, policy enforcement, and cost visibility."
([source](https://www.openhands.dev/blog/open-source-ai-coding-agents))

So the capability kestrel's north-star audience needs most is, on the largest open platform, positioned
as **Enterprise**. Treat this carefully: that page is OpenHands' own marketing and its comparison table
is self-serving. What is in the MIT core versus the Enterprise tier should be confirmed against the
repository before kestrel makes any public claim about it. **Undetermined.**

## Broader field

From secondary surveys ([aq.dev](https://aq.dev/guides/self-hosted-ai-coding-agents/),
[Augment Code](https://www.augmentcode.com/tools/open-source-agent-orchestrators)) — treated as leads,
not evidence:

- **Coding agents, not factories**: OpenCode, Cline, Aider, Goose, Kilo Code, Tabby. Self-hostable,
  mostly Apache-2.0/MIT, but no event ingestion or control plane. Not competitors; several are potential
  *runtimes*.
- **Generic orchestrators**: LangGraph, CrewAI, Dify, Sim, Letta. Docker/k8s self-hostable, but general
  agent frameworks rather than SDLC factories. Overlap, not occupation.
- **Trigger.dev** — a background job platform with cron/webhook/event triggers, positioned for
  long-running agent tasks. Adjacent infrastructure kestrel might use rather than replace.
- **OpenClaw / NemoClaw** — a self-hosted assistant and a control plane for agent fleets on
  Docker/Kubernetes. Worth a closer look later; not examined against primary sources here.

## What this means for kestrel's front page

The lead has to change. Three honest options, strongest first:

1. **Infrastructure portability + governance in the open core.** The project that matches kestrel's shape
   is Cloudflare-locked and explicitly single-tenant with a shared-credential security model; the project
   with real governance puts it behind an Enterprise tier. **Nobody is offering a factory you can run on
   your own infrastructure — docker-compose, Kubernetes, your cloud — with multi-tenancy and audit in the
   open core.** That is a real, defensible, and much narrower gap than "nothing exists."
2. **Pluggability as the thesis.** kestrel's committed posture — pluggable compute, storage, and model —
   is stronger than Open-Inspect's Cloudflare coupling. Weaker as a headline: it describes *how* it is
   built, not *why* anyone needs it.
3. **Workflows.** No open project surveyed offers workflow composition over sessions. Genuinely
   unoccupied, but unproven as something people are asking for.

The category being crowded is not, by itself, an argument against kestrel — it is evidence the category is
real, which the original thesis could not have claimed. But the differentiation now has to be earned on
capability, not on being the only one there.

## Open questions

- What exactly is in OpenHands' MIT core versus Enterprise? The governance boundary is load-bearing for
  option 1 and is currently sourced only to OpenHands' own marketing.
- How good is Open-Inspect *in practice*? 105 open issues on 2.7k stars, and its multiplayer and durable
  sessions are built on the same lossy opencode event stream kestrel found in [#2](https://github.com/jtmthf/kestrel/issues/2) — worth reading how they handled it.
- Does Open-Inspect's Cloudflare coupling run deeper than deployment (Durable Objects are a genuinely
  distinctive primitive), and is that a moat or a millstone?
