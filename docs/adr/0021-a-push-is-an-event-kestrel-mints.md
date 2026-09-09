# A push is an Event kestrel mints, and authority stays in the command

An operator can hand kestrel one issue by id, or a raw prompt, without waiting for an external system
to produce an event. kestrel mints an internal event for it. The authority to start that work comes
from the authenticated request, never from the event the request minted.

Every path into kestrel today is a pull: an external system records an event, a trigger matches it, a
firing opens a session. There is no push. Confirming that a trigger's guard works, re-driving a
session deliberately, or handing an agent an ad-hoc task all require finding an external system to
poke and waiting for a poll.

## Why an event, rather than a side door

`CONTEXT.md` said kestrel "translates nothing into a vocabulary of its own, and mints one type only,
for its own schedules elapsing," which reads as a prohibition and is not one — a schedule elapsing is
already an internal event kestrel mints. The clause's subject is **translation**: do not restate
GitHub's vocabulary in kestrel's. An event kestrel mints for something kestrel itself did is not
translation, so the clause is reworded rather than excepted.

Making a push an event means the trigger machinery, the correlation, the audit record and the outcome
round-trip all work unchanged instead of growing a second path beside them. `dispatch #109` should be
indistinguishable in its effects from labelling issue #109 — that is the entire reason for the
choice, and it is what makes a push usable as the dry run that confirms a guard rather than a
separate feature that approximates one.

## Where the authority went

[ADR-0013](0013-an-event-supplies-data-never-authority.md) says an event supplies data and never
authority: brief and correlation are rendered from an event, while agent, workspace, model and policy
are named in a declaration a human reviewed. A pushed raw prompt names its agent and workspace
directly, which looks like a violation.

It is not, once the two halves are separated. The **authenticated request** carries the authority;
the **event** it mints carries only data. This is already true of the shape kestrel has:
[ADR-0015](0015-the-cli-is-a-client-not-a-role.md) makes the CLI a client that asks and the control
plane the thing that decides, so authority genuinely lives in the request rather than in anything
downstream of it. The invariant stays literally true, and gains a second clause saying an event
kestrel minted for itself is no more trusted than one an integration delivered.

Restating the invariant as "events *from integrations* supply no authority" was rejected. It weakens
the rule to "trust what comes from inside," which is the sentence nobody wants on the page when the
second internal event source appears.

## Consequences

- **Two commands, not one.** A raw prompt has no cause and invents none: it opens a session naming
  agent, workspace and brief. Dispatching an issue names an existing **Trigger** and supplies the
  event to run it against, reusing that trigger's brief template, agent and workspace, and producing
  exactly what a real firing would.
- **A dry run and a deliberate push are the same mechanism.** `trigger declare` has no way to ask
  what it would do, and on 2026-09-09 one declaration opened eleven unrequested sessions in a third
  of a second. Running a trigger against a supplied event, without starting work, is that missing
  command pointed the other way.
- **A pushed session has an Outcome, addressed to what its event names.** `dispatch #109`
  round-trips to the issue exactly as labelling it would. A raw prompt has no subject and no outcome,
  which `CONTEXT.md` already handles: a session no event started has none, and one whose event
  reaches no integration has nowhere to say it.
- **A firing budget still bounds a push.** Nothing about arriving by hand exempts work from the caps
  that bound work arriving by poll.
