# An event supplies data, never authority

A trigger renders the branch, the brief and the correlation key from the event that fired it. It
never renders the agent, the workspace, the model or the policy: those are named in the declaration
a human reviewed and applied. The event is attacker-controlled text, and the line between the two
lists is the line between what it may describe and what it may choose.

## Considered options

**Rendering the agent from the event** is the natural next step once branches render, and it is
attractive: `agent: {{ event.data.labels[0].name }}` routes `bug` to one agent and `security` to
another with one declaration instead of three. It is also a privilege-escalation primitive — anyone
who can label an issue picks which agent's policy and credentials the run gets. Rendering the model
is the same mistake with a different payoff: an attacker who picks the model picks the most
expensive one, and combined with opening sessions at will that is a cost-exhaustion attack with no
policy layer beneath it to stop. Label-based routing, if it is wanted, is three triggers with three
filters, each of which someone approved.

## Consequences

- **This does not solve prompt injection, and nothing at this layer will.** A GitHub issue body is
  written by anyone on the internet and is rendered into a brief handed to an agent holding the
  organization's provider credential and a checkout with push rights. Injection is *bounded* at the
  policy layer, which is `0.4`. What this decision buys is narrower and worth having: a stranger can
  never choose which agent runs, on which repositories, with which model.
- **A trigger may filter on the actor**, so declining to fire on strangers at all is one clause —
  GitHub hands over `author_association`. `kestrel trigger apply` warns by name when a trigger's
  filter admits actors outside the organization.
- **Until `0.4`, a trigger that fires on public content is an unsupervised agent with your
  credentials on your repository.** That should be a decision someone made on purpose rather than
  discovered, which is what the warning is for.
- **A firing feeds a session but never chooses its agent.** A trigger's agent applies on open only;
  a session's agent is fixed for its life, because the transcript's meaning depends on it being one
  actor throughout.
