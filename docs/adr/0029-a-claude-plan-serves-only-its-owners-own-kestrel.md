# A Claude plan serves only its owner's own kestrel

A Claude Subscription Profile ([ADR-0025](0025-subscription-profiles-are-personal.md)) is permitted
in one shape: a person runs kestrel for themselves, holds a token they minted with `claude
setup-token`, and names it only on their own Sessions. The Run spawns `claude-agent-acp`, which runs
the unmodified Claude Code binary through the Agent SDK. kestrel does not offer Claude plan access to
anyone else: not a profile whose owner is not the operator, not a shared Organization profile, and
not a kestrel hosted for other people. Those use an Anthropic API key, a supported cloud provider,
or prior approval from Anthropic.

**The authority.** Anthropic's Claude Code
[legal and compliance page](https://code.claude.com/docs/en/legal-and-compliance), checked
23 September 2026, is the most specific guidance and is the one this rests on:

> Anthropic does not permit third-party developers to offer Claude.ai login into their own
> applications, or to route requests through Free, Pro, or Max plan credentials on behalf of their
> users. Moreover, developers may not collect, store, or intermediate Claude.ai credentials or
> session tokens — sign-in to a Claude account must complete through Anthropic's own flow.

> Nor does it prevent an end user from signing in to the unmodified Claude Code binary with their
> own Claude subscription, including where a platform hosts Claude Code […]

> Advertised usage limits for Pro and Max plans assume ordinary, individual usage of Claude Code
> and the Agent SDK.

The [Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview) says the same of
products: without prior approval, a third party may not offer claude.ai login or rate limits. The
[June 2026 help article](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan)
that [the research note](../research/subscription-auth-acp.md) found in tension with it is about
billing, not permission: it paused a separate Agent SDK credit, and SDK use still draws from the
plan's limits. The [authentication page](https://code.claude.com/docs/en/authentication#generate-a-long-lived-token)
documents `claude setup-token` for scripts and CI, completed through Anthropic's own browser flow.

**Why the personal shape is permitted.** Every restriction is on a developer acting for its users:
offering a login, routing their requests, holding their tokens. In a personal deployment there are
no such users. The person signs in through Anthropic's flow, stores their own token in
infrastructure they run, and spends it on their own work in the unmodified binary, which the page
names as permitted. Three lines of the same page cut against this, and each is answered by who the
user is:

- *Developers building products […] including those using the Agent SDK, should use API key
  authentication.* The operative prohibition that follows it is about users' credentials, and the
  page separately names ordinary, individual Agent SDK use as what plan limits assume.
- *Developers may not collect, store, or intermediate Claude.ai credentials.* kestrel does store
  the token, sealed in its database. In a personal deployment the one storing it is its owner,
  as a secrets manager would. Holding anyone else's is exactly this prohibition.
- *The unmodified Claude Code binary.* `claude-agent-acp` drives the Agent SDK, which the
  [overview](https://code.claude.com/docs/en/agent-sdk/overview) describes as "a library that runs
  the Claude Code binary". kestrel pins the adapter from npm and patches nothing.

This is a reading of the guidance, not an approval from Anthropic. The
[contact-sales link](https://www.anthropic.com/contact-sales) on that page is the way to settle
anything beyond it.

## Considered options

- **Treat a successful Claude call as permission.** Rejected by the 0.2 specification (#165): the
  smoke check proves that the path works, not that it is allowed.
- **Drop Claude plan support; require an API key.** Unambiguous, but it forbids the one use the
  guidance names as permitted, and dogfooding on the maintainer's own plan is the point of 0.2.
- **Allow Claude profiles generally.** This is the product offering Claude plan login that
  Anthropic forbids.

## Consequences

- Nothing in kestrel enforces the boundary. Naming a profile is the only authorization there is
  (ADR-0025), so this is an operator's obligation, which `USAGE.md` states where a Claude token is
  held.
- Multiuser Subscription Profiles, deferred by #165, cannot include Claude plans unless this
  decision is revisited with Anthropic's approval.
- "Ordinary, individual usage" bounds how hard a Claude profile is driven. A kestrel that fans one
  plan out across many parallel Runs is outside it, even when the plan's rate limits allow it.
- The guidance is vendor policy and changes without notice. Check it again before a release that
  relies on this ADR.
