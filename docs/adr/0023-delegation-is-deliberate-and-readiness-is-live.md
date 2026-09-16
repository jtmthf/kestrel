# Delegation is deliberate and readiness is live

An external work item starts a Session only after an authorized assignment, agent delegation,
mention, or explicit dispatch. `ready-for-agent` remains triage metadata, not a Kestrel start
signal: many ready-labelled issues are blocked, and a label says nothing about whether its owner
has handed the work to Kestrel. Before an automatic start, the integration fetches current issue
state and blockers; unknown readiness holds the request. An authorized explicit instruction may
override a blocker, with that choice recorded.

Event filters remain the source's vocabulary, as [ADR-0012](0012-trigger-filters-are-a-dialect-not-a-language.md)
requires. The live readiness check is a separate gate because GitHub, Jira, and Linear express
blockers differently and an old event cannot prove their current state. Held requests are
rechecked on relevant changes and periodically so a missed webhook does not strand work. Harness
labels select only among agents the operator already allowed; an untrusted issue or label cannot
grant new authority, preserving [ADR-0013](0013-an-event-supplies-data-never-authority.md).
