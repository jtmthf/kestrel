# A Workspace fixes the place, a Session chooses the Agent

A workspace fixes its project, environment and instance for its life; each session in it names its
own agent. A trigger whose firing correlates to an open workspace declares whether it continues the
waiting session or starts a new session with the trigger's own agent, and a workflow may enqueue a
session into its own workspace as well as open a new one.

This relaxes the rule that a workspace's agent was fixed when it opened. That rule was written when a
second session in a workspace existed only to recover work a first one left unfinished. The
legitimate second sessions are different jobs — review, merge conflicts, a CI failure — and each
wants a fresh context and often a different model or instructions: an automated review from the
agent that wrote the code reads its own reasoning back as confirmation. Forcing those jobs into a
new workspace would give them a new instance and checkout, able to see only pushed work.

The environment is chosen when the workspace opens rather than per session, amending
[ADR-0017](0017-the-environment-is-declared-the-instance-is-provisioned.md): the instance belongs to
the workspace, so the declaration it is provisioned from does too. It defaults from the project,
which a trigger or dispatch may override, rather than from the agent: a toolchain follows the
repositories, and the agent of the opening session may never run in the workspace again.

## Considered options

**Inferring continue or new session** from whether the trigger's agent matches the waiting
session's. Rejected: a second CI failure routed to the same CI-fixing agent still wants a fresh
context, and authority in kestrel comes from what a reviewed declaration says, not from what kestrel
guesses.

## Consequences

- One unfinished session per workspace still holds
  ([ADR-0014](0014-concurrency-lives-across-sessions-never-within-one.md)). A firing that starts a
  new session waits for the unfinished one to let go.
