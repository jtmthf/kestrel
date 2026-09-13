# CloudEvents is the Event's shape, and kestrel translates no vocabulary

An Event is a CloudEvent — `id`, `source`, `specversion`, `type`, `subject`, `time`, `data` — and it
is named in the vocabulary of the system that produced it. kestrel initially accepts
`specversion: "1.0"` and requires its adapters to supply `time`, while `subject` remains optional.
`com.github.check_run.completed` reaches a trigger spelled the way GitHub spells it, and kestrel
owns no `dev.kestrel.ci.failed` to map it onto.

## Considered options

**A normalized taxonomy**, where every adapter maps its source's events into kestrel's own names, is
the alternative, and it buys one real thing: a trigger that catches CI failures from GitHub Actions
and Buildkite alike, written once. It was rejected because the set it must cover is unbounded —
assignments, workflow failures, CVEs, merge-queue ejections, and whatever the next integration
supports — and every entry needs kestrel to hold an opinion, forever, about a system it does not
own. It also falsifies the roadmap's depth-first bet: "the named integrations are adapters over the
generic webhook" is untrue the moment each adapter must translate into a private vocabulary.

The **CloudEvents wire format** is accepted on ingest, in both HTTP modes, because a real population
of producers already speaks it — Knative, Event Grid, Eventarc, Argo Events, Tekton, Debezium — and
parsing two header conventions is nearly free. Everything past that is refused: no protocol bindings
beyond HTTP, and no Subscriptions or Discovery API, because implementing those makes kestrel an
event broker competing with the brokers people would point at it. kestrel emits no CloudEvents
either, which would be strange next to a refusal to own event types coming in.

## Consequences

- **A trigger is written against one source's names**, so catching the same class of event from two
  providers is two match clauses. That cost is paid by the few, rather than a mapping burden paid by
  every adapter.
- **`source` is the external resource, not the integration**, so an event dedups within its
  organization whether kestrel learned it by polling or by webhook, and a trigger survives that
  migration unchanged. Separate organizations retain separate copies of the same producer event so
  event data never crosses the tenant boundary. For the same reason an adapter mirrors the
  provider's webhook vocabulary even when it polled a different endpoint entirely and no webhook
  was involved.
- **The CloudEvents `id` is the Event's producer-assigned identifier.** kestrel may assign a local
  record identifier for references, but that value is not the Event's `id` and is named separately.
- **Triggers match on `source`, never on an integration**, which leaves an integration as a
  transport and a credential and nothing else.
- **An adapter refuses a poll when it cannot derive every required CloudEvents attribute**, and the
  poll watermark does not advance. Skipping a malformed event would make that loss permanent.
- **An Event whose serialized `data` exceeds the ingest limit is refused without storing its
  payload**, and the poll watermark advances so one deterministic policy rejection cannot block the
  stream. The refusal remains visible to an operator.
- **Provider-specific interpretation of `data` belongs to the adapter**, while the Event exposes
  only the CloudEvents envelope and the producer's payload.
- **kestrel mints exactly one type of its own**, for a schedule elapsing, so that scheduled work is
  recorded, filterable and dry-runnable on the one firing path rather than a second one.
