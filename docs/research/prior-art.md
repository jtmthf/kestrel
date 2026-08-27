# Prior art teardown

Resolves [#3](https://github.com/jtmthf/kestrel/issues/3). Public sources read 2026-08-26. Claims are
attributed; inference is labelled. Where a system is opaque, this says so rather than filling the gap.

## Headline: durable multiplayer sessions are not a differentiator

Charting proposed durable multiplayer sessions as kestrel's supporting differentiator, on the belief
that the incumbents are one-shot. **That belief is wrong for the most relevant system.**

**Ramp Inspect already does exactly this**, on an architecture strikingly close to kestrel's plan, and
[Open-Inspect](https://github.com/ColeMurray/background-agents) has open-sourced a clone of it under MIT
([#5](https://github.com/jtmthf/kestrel/issues/5)).

The one-shot belief holds only for **Stripe Minions**.

## Ramp Inspect — the closest analogue

Source: [rywalker.com/research/ramp-inspect](https://rywalker.com/research/ramp-inspect). Secondary
research write-up, not Ramp's own documentation; treat details as reported rather than confirmed.

- **Architecture**: control plane on **Cloudflare Durable Objects** (per-session state), data plane on
  **Modal sandboxes**. Reported to enable "rapid session startup and unlimited concurrency."
- **Agent runtime**: **OpenCode**, supporting "all frontier models" via MCP plus custom tools.
- **Triggers**: Slack bot, web, Chrome extension, voice, mobile. Sandboxes begin "spinning up" warmly
  **when the user starts typing**.
- **Session model**: **durable and multiplayer** — teams "observe and guide agents in real-time," and
  agents "spawn sub-agents for research or decomposition." Images rebuilt every 30 minutes from snapshots.
- **Governance**: PRs "opened on behalf of user token, preventing self-approval"; human review mandatory.
- **Scale**: as of February 2026, over **half of Ramp's merged pull requests**, up from ~30% in January —
  "organic adoption without mandates."
- **Stated limits**: "frontier models still hallucinate and require oversight"; it is "internal tooling,
  not a product for sale," and replicating it requires "significant work."

**This is kestrel's plan, already built and already cloned into MIT.** The architecture kestrel sketched
at charting — control plane + sandboxed data plane + opencode + multiplayer sessions — is a described,
working, measured system. That is validation of the design and elimination of the novelty claim at once.

## Stripe Minions — the genuine one-shot

Source: [Alistair Gray's Background Agents Summit session](https://background-agents.com/summit/sessions/alistair-gray/).

- **Triggers**: Slack, Jira tickets, web UI.
- **Environment**: "Human developer infrastructure becomes the agent runtime" (06:47) — a reproducible dev
  box per agent run, against a 30-million-line codebase. Sourcegraph-powered retrieval.
- **Determinism**: "Blueprints, checks, and PR creation" (09:36) — reproducible step-by-step execution
  rather than freeform generation.
- **Session model**: **one-shot** — trigger, work, PR, human review. No durable session persistence
  mentioned. *(Absence of mention is not proof of absence — but it is consistent with Minions' framing.)*
- **Governance**: mandatory code review; "risk profiles, and code ownership" adapted to agent-authored
  code (14:15).
- **Scale**: "thousands of agent-written PRs a week, with humans still reviewing the code" (00:00).

**Lesson kestrel should take:** the emphasis on *blueprints* and *agent-ready environments* as
prerequisites, not automatic. Minions' reliability comes from constraining the agent's path, not from a
better model.

## Abnormal Nora — the governance high-water mark

Source: [builders.abnormal.ai](https://builders.abnormal.ai/p/nora-our-first-agent-employee).

- **Triggers**: interactive Slack @-mentions; **proactive** monitoring of help channels answering
  "hundreds questions per day without being @-mentioned"; cron; event-driven Jira ("ticket triggers
  triage, research, planning, implementation, staging verification, and PR creation").
- **Environment**: two tiers — a lightweight always-on **code interpreter**, and a heavyweight
  Modal-based **devbox** that "mirrors what a developer has on their laptop." Notably it delegates repo
  work to **Claude Code CLI** rather than executing it itself.
- **Session model**: human intervention supported; trace logs exportable so "the agent can self-debug";
  corrections saved as a **tip** and injected into future prompts for that persona.
- **Governance — the most sophisticated model surveyed:**
  - Nora has "its own Okta profile, a Google Workspace account, ... provisioned the way we'd provision a
    new hire" — "a first-class non-human identity in our organization."
  - **Permission intersection**: "the effective permissions are the intersection of what the service
    account can access and what the user themselves should have access to."
  - **Contextual write scoping**: "a request from a help channel gets Jira ticket creation. A request
    from an engineering workflow gets GitHub PR creation."
  - **Taint tracking**: "if the agent reads a sensitive data source, certain write actions become
    unavailable for the remainder of that run."
- **Scale**: 300+ personas, 1,000+ Slack requests/day, 200+ PRs/day.

**This is the most valuable find for kestrel's positioning.** Permission intersection, contextual write
scoping, and read-taint restriction are concrete, describable governance primitives — and **nothing open
implements them**. Open-Inspect is explicitly the opposite: single-tenant, with shared GitHub App
credentials where "any user of the system can access any repo the App has access to"
([#5](https://github.com/jtmthf/kestrel/issues/5)).

## Others, briefly

From [background-agents.com/llms.txt](https://background-agents.com/llms.txt) — Ona's own framing, so
treat as positioning rather than neutral reporting:

- **DoorDash Flux** — "130,000 automated engineering tasks in one month," "more than 25,000 code reviews
  each week." Architecture not publicly documented in sources read.
- **Ona** — publisher of the background-agents thesis; acquired by OpenAI. Its five primitives (sandboxed
  execution, runtime governance, context/connectivity, triggers, fleet coordination) are the framing
  kestrel adopted.
- **Block Builderbot**, **Sierra** — not examined in depth.

## What this means for kestrel

1. **Drop durable multiplayer sessions as a differentiator.** Ramp does it; Open-Inspect open-sourced it.
   It is now **table stakes** — kestrel must have it to compete, and gets no credit for it.
2. **Governance is the real opening.** Nora shows what good looks like, Ramp keeps it internal, and
   Open-Inspect explicitly punts on it. Permission intersection, contextual write scoping, and taint
   tracking in an **open, multi-tenant** control plane is unoccupied ground and matches the north-star
   audience exactly.
3. **The architecture is validated.** Control plane + sandboxed data plane + opencode is what the two
   most successful systems converged on independently. kestrel is not taking an architectural risk.
4. **Constrained execution beats clever agents.** Minions' blueprints and Ramp's tool-verified work both
   point the same way. Relevant to how workflows should be framed.
5. **Warm-start matters.** Ramp provisions sandboxes when the user *starts typing*. Latency is a UX
   feature, and it interacts directly with the backend cold-start figures in
   [#4](https://github.com/jtmthf/kestrel/issues/4).

## Reliability caveats

- Ramp Inspect and Open-Inspect details come from **secondary research write-ups**, not first-party
  engineering documentation. Ramp has published little directly.
- The Minions session was read via a summary of its page, not by watching the recording; timestamps are
  as given on the session page.
- DoorDash Flux, Block Builderbot and Sierra were **not** examined in depth.
- Scale figures throughout are self-reported by the companies and measure different things.
