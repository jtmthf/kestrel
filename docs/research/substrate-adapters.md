# The substrate-adapter capability matrix

Resolves [#14](https://github.com/jtmthf/kestrel/issues/14). Primary docs read 2026-08-27. Open-Inspect
source read at `ColeMurray/background-agents@main`.

## The finding: the design target is Azure Container Apps, not Lambda

[#10](https://github.com/jtmthf/kestrel/issues/10) set multiplayer's design target at "Lambda's floor,"
on the strength of API Gateway's two-hour WebSocket cap. That is not the floor. **Azure Container Apps
terminates an ingress connection after 240 seconds and the value is not configurable in the default
ingress mode.** The ingress docs state it flatly, in the same list that promises WebSocket support:

> "Support for WebSocket and gRPC"
>
> "Request time out is 240 seconds"
>
> — [Ingress in Azure Container Apps](https://learn.microsoft.com/en-us/azure/container-apps/ingress-overview)

The only escape is **premium ingress mode**, whose advanced settings table gives an "Idle request timeout"
with a **minimum of 4 minutes, a maximum of 30 minutes, and a default of 4**
([source](https://learn.microsoft.com/en-us/azure/container-apps/ingress-environment-configuration)).
Premium ingress requires a dedicated workload profile that "must not be the Consumption workload profile"
and "must not be shared with container apps or jobs" — i.e. it is a paid, always-on resource. So the
honest ACA ceiling is **four minutes**, and thirty at best.

Ranked, the realtime ceilings across the eight targets are:

| Substrate | Connection ceiling | Source |
|---|---|---|
| **Azure Container Apps** | **240 s** (default ingress); 30 min max on premium | ingress docs, above |
| Vercel | 300 s (Hobby, hard); 800 s max Pro/Ent; 1800 s in beta | [limits](https://vercel.com/docs/functions/limitations) |
| Cloud Run | 300 s default, "up to 60 minutes (3600 seconds)" | [request timeout](https://docs.cloud.google.com/run/docs/configuring/request-timeout) |
| ECS (behind ALB) | idle timeout 60 s default, "valid range is 1 to 4000 seconds" | [ALB attributes](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/edit-load-balancer-attributes.html) |
| AWS Lambda | "Connection duration for WebSocket API — 2 hours", idle 10 minutes | [API GW quotas](https://docs.aws.amazon.com/apigateway/latest/developerguide/apigateway-execution-service-websocket-limits-table.html) |
| Cloudflare | no documented duration cap; hibernation instead | [DO WebSockets](https://developers.cloudflare.com/durable-objects/best-practices/websockets/) |
| Kubernetes / compose | whatever the operator's ingress allows | operator-owned |

**The consequence for kestrel is larger than a number.** Designing to two hours permits a connection that
outlives a typical agent run; designing to four minutes does not. At 240 seconds, **the connection is
never the unit of session continuity** — a human watching an agent work will be disconnected dozens of
times per run on the substrate with the tightest cap. Every client must therefore reconnect and *resume
from a cursor*, and every server-side push must be durable *before* it is delivered, or it is lost in the
gap. That is not a Lambda-shaped design with margin; it is a stricter design than Lambda forces.

Two vendors say this in their own docs, unprompted:

> "WebSockets clients connecting to Cloud Run should handle reconnecting to the server if the request
> times out or the server disconnects."
> — [Cloud Run WebSockets](https://docs.cloud.google.com/run/docs/triggering/websockets)

> "New WebSocket connections are not guaranteed to reach the same Vercel Function instance. If a client
> reconnects, it may connect to a different instance. ... Store durable state, presence, counters, rooms,
> and pub/sub coordination in an external data store instead of relying on in-memory variables."
> — [Vercel WebSockets](https://vercel.com/docs/functions/websockets)

This *strengthens* [#10's](https://github.com/jtmthf/kestrel/issues/10) ruling rather than upsetting it.
One uniform multiplayer promise designed to the weakest transport is still right; the weakest transport is
just weaker than the ticket assumed, and the number that goes in the design doc is **240 seconds**.

One small correction while here: [#10](https://github.com/jtmthf/kestrel/issues/10) records API Gateway's
integration timeout as 30 s. The quota table says **"50 milliseconds - 29 seconds for all integration
types."**

---

## Why Open-Inspect's `platform-ports.ts` is insufficient

Read before proposing a replacement, as [#14](https://github.com/jtmthf/kestrel/issues/14) asked. The file
is `packages/control-plane/src/platform-ports.ts` and it is 28 lines. In full, minus imports:

```ts
/** Capability consumed by application services that defer background work. */
export interface BackgroundTasks {
  submit(
    task: () => Promise<unknown>,
    metadata: { name: string; context?: Record<string, unknown> }
  ): void;
}

/** Access the runtime's single scheduled wake-up. */
export interface AlarmScheduler {
  schedule(at: number): Promise<void>;
  cancel(): Promise<void>;
  current(): Promise<number | null>;
}

// Keep platform compatibility checked at the boundary rather than widening every consumer.
type _AssertExtends<A extends B, B> = A;
type _FetcherSatisfiesFetchClient = _AssertExtends<Fetcher, FetchClient>;
```

The craft is not in question — the Durable Object class is deliberately thin and application wiring is
separated into `createSessionRuntime`, which is real discipline. **The failure is scope and framing, in
five specific ways.**

**1. It ports two capabilities and neither is a hard one.** Of the six capability areas this ticket
surveys, storage and realtime transport are absent from the port entirely. They sit directly on Durable
Object semantics — `this.sql = ctx.storage.sql` and `this.ctx.acceptWebSocket(ws, tags)` — with `session/`
running to roughly 36,700 lines and `hibernat` appearing throughout it
([#5 addendum](https://github.com/jtmthf/kestrel/blob/research/oss-landscape/docs/research/oss-landscape.md)).
A port that abstracts the parts that were already portable is an untested abstraction, not a contract.

**2. `AlarmScheduler` puts the mechanism's cardinality in the contract.** The doc comment —
*"Access the runtime's single scheduled wake-up"* — is Cloudflare's model stated as a universal:

> "Each Durable Object is able to schedule a single alarm at a time by calling `setAlarm()`."
> — [Durable Object alarms](https://developers.cloudflare.com/durable-objects/api/alarms/)

Note what the interface does *not* take: an identifier. `schedule(at)`, `cancel()`, `current()` are
singleton operations, implementable only where the runtime has already given you a per-session actor with
its own timer. Every other scheduler surveyed here is **global, not per-session** — EventBridge Scheduler,
Cloud Scheduler, Vercel Cron, Kubernetes CronJob, Container Apps scheduled jobs. Implementing this port on
any of them means building a per-session due-time registry *underneath* the port, which is the entire hard
part, and the port offers no help with it. **A port should name the intent (a session is due at time T),
not the mechanism (there is one alarm slot).** This is the specific mistake #14 flagged, and it is real.

**3. `BackgroundTasks.submit` promises something no substrate but Cloudflare can keep.** The only
implementation is `context.waitUntil(pending.catch(logFailure))`
(`src/cloudflare/background-tasks.ts`), and Cloudflare caps `waitUntil` at 30 seconds past the response.
The interface returns `void`, so there is no completion, no durability, and no failure channel but a log
line — the doc comment is explicit that "a synchronous throw is absorbed and logged exactly like a
rejection." On Lambda the execution environment is frozen after the response, so the identical call
silently means *nothing runs*; on Cloud Run it means nothing runs unless the service is on instance-based
billing. **The port's semantics change per substrate without changing type**, which is the worst
property an interface can have.

**4. It is type-checked against exactly one platform.** `_FetcherSatisfiesFetchClient` asserts
Cloudflare's `Fetcher` satisfies the port. This is the rule of two from
[#10](https://github.com/jtmthf/kestrel/issues/10) failing in miniature: an interface driven once.

**5. It is scoped to a session runtime, not to a deployment.** Nothing in it can express the fact that
distinguishes these eight targets from one another — whether a long-lived process exists at all. The
alarm handler's own comment gives the assumption away: *"alarms run outside any request, so there is no
request correlation"* (`session/alarm/handler.ts`). That is true on a Durable Object and false on Vercel,
where an alarm *is* a request.

---

## Capability matrix

| | Durable state | Realtime transport | Scheduled wake-up | Background execution | Container image | Default compute pairing |
|---|---|---|---|---|---|---|
| **docker-compose** | Docker volumes on host disk | Server owns socket; single node, no bus needed | None native — kestrel's own timer in-process | Unbounded; process outlives everything | Yes (native) | Docker daemon |
| **Kubernetes** | PVC / StatefulSet; Postgres | Server owns socket; **cross-node fan-out needs a bus** | `CronJob`, "about one Job" per cycle, may fire twice or none | Unbounded; `activeDeadlineSeconds` optional | Yes (native) | Kubernetes Jobs |
| **ECS (Fargate)** | Ephemeral 20–200 GiB per task; EFS/EBS for durable | Server owns socket behind ALB; **idle timeout 60 s default, 4000 s max** | EventBridge Scheduler → `RunTask` | Unbounded per task | Yes (native) | ECS `RunTask` on Fargate |
| **Cloud Run** | **No disk** — filesystem is in-memory and counts against RAM; GCS/NFS volumes, Cloud SQL | Native WebSocket, no config; capped at request timeout (60 min max); **affinity is best-effort** | Cloud Scheduler (at-least-once) | Only with instance-based billing; idle instance "can be shut down at any time" | Yes (native) | Cloud Run jobs, gen-2 execution env (microVM) |
| **Azure Container Apps** | Ephemeral 1–8 GiB by vCPU; Azure Files (SMB/NFS) for durable | WebSocket + gRPC supported; **240 s request timeout**, 30 min on premium ingress | Scheduled jobs, 5-field cron, UTC | Jobs (`replicaTimeout`); apps need min-replicas ≥ 1 | Yes (native) | ACA **dynamic sessions**, custom container pool (Hyper-V) |
| **Vercel** | **No disk that survives**; bring-your-own Postgres/Redis from the Marketplace | **Socket pinned to one instance**; dies at `maxDuration` (300 s Hobby / 800 s Pro) | Vercel Cron — **Hobby: once per day, ±59 min** | `waitUntil`; hard-bounded by `maxDuration` | Yes — `Dockerfile.vercel` + Vercel Container Registry | Vercel Sandbox (Firecracker) |
| **AWS Lambda** | `/tmp` 512 MB–10,240 MB, per-instance only; DynamoDB/RDS | **Gateway owns the socket**; stateless per-message invoke, push via `@connections`; 2 h cap, 10 min idle | EventBridge Scheduler (at-least-once, 270+ services) | **None after response** — 900 s ceiling inside one invoke | Yes — 10 GB uncompressed image | **Lambda MicroVMs** (8 h max, ARM64) |
| **Cloudflare** | DO SQLite 10 GB/object; D1, KV, R2 | **Hibernatable WebSocket** on a Durable Object; "In-memory state is reset" | DO alarm (one per object) + Cron Triggers | `waitUntil` 30 s past response; alarms/queues 15 min | Yes — Cloudflare Containers (Workers Paid) | Cloudflare Containers via Sandbox SDK (**preview**) |

### Sources for the matrix

Docker [volumes](https://docs.docker.com/engine/storage/volumes/) ("A volume's contents exist outside the
lifecycle of a given container"). Kubernetes [CronJob](https://kubernetes.io/docs/concepts/workloads/controllers/cron-jobs/)
and [Job](https://kubernetes.io/docs/concepts/workloads/controllers/job/). ECS
[Fargate task storage](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/fargate-task-storage.html),
[scheduling](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/scheduling_tasks.html),
[ALB attributes](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/edit-load-balancer-attributes.html).
Cloud Run [container contract](https://docs.cloud.google.com/run/docs/container-contract),
[request timeout](https://docs.cloud.google.com/run/docs/configuring/request-timeout),
[WebSockets](https://docs.cloud.google.com/run/docs/triggering/websockets),
[CPU allocation](https://docs.cloud.google.com/run/docs/configuring/cpu-allocation),
[jobs](https://docs.cloud.google.com/run/docs/create-jobs),
[execution environments](https://docs.cloud.google.com/run/docs/about-execution-environments).
Container Apps [ingress](https://learn.microsoft.com/en-us/azure/container-apps/ingress-overview),
[environment ingress](https://learn.microsoft.com/en-us/azure/container-apps/ingress-environment-configuration),
[jobs](https://learn.microsoft.com/en-us/azure/container-apps/jobs),
[storage mounts](https://learn.microsoft.com/en-us/azure/container-apps/storage-mounts),
[dynamic sessions](https://learn.microsoft.com/en-us/azure/container-apps/sessions).
Vercel [limits](https://vercel.com/docs/functions/limitations),
[WebSockets](https://vercel.com/docs/functions/websockets),
[cron pricing](https://vercel.com/docs/cron-jobs/usage-and-pricing),
[Sandbox](https://vercel.com/docs/sandbox), [Docker](https://vercel.com/kb/guide/docker).
Lambda [quotas](https://docs.aws.amazon.com/lambda/latest/dg/gettingstarted-limits.html),
[API Gateway WebSocket quotas](https://docs.aws.amazon.com/apigateway/latest/developerguide/apigateway-execution-service-websocket-limits-table.html),
[EventBridge Scheduler](https://docs.aws.amazon.com/scheduler/latest/UserGuide/what-is-scheduler.html).
Cloudflare [Workers limits](https://developers.cloudflare.com/workers/platform/limits/),
[DO limits](https://developers.cloudflare.com/durable-objects/platform/limits/),
[alarms](https://developers.cloudflare.com/durable-objects/api/alarms/),
[WebSocket hibernation](https://developers.cloudflare.com/durable-objects/best-practices/websockets/),
[storage options](https://developers.cloudflare.com/workers/platform/storage-options/),
[Cron Triggers](https://developers.cloudflare.com/workers/configuration/cron-triggers/),
[Containers](https://developers.cloudflare.com/containers/) and
[container limits](https://developers.cloudflare.com/containers/platform-details/limits/),
[Sandbox SDK](https://developers.cloudflare.com/sandbox/).

---

## The capabilities, one at a time

### Durable state — local disk is not in the contract

Four of eight have durable local disk (compose, Kubernetes via PVC, ECS via EFS/EBS, Container Apps via
Azure Files). Four do not, and two say so explicitly. Cloud Run's runtime contract is the sharpest:

> "It is an in-memory file system, so writing to it uses the instance's memory."
>
> "Data written to the file system doesn't persist when the instance stops."
>
> — [Container runtime contract](https://docs.cloud.google.com/run/docs/container-contract)

Lambda's `/tmp` is "Between 512 MB and 10,240 MB" but belongs to one execution environment, not to the
function. Vercel documents no writable persistent path at all. Cloudflare Workers have no filesystem;
storage is [KV, R2, D1, Durable Objects, Queues, Hyperdrive](https://developers.cloudflare.com/workers/platform/storage-options/),
and only Durable Objects and D1 are strongly consistent.

Even where disk exists it is often the wrong shape: ECS Fargate gives "a minimum of 20 GiB of ephemeral
storage ... up to a maximum of 200 GiB," but the container image is stored on it and it dies with the
task. Container Apps allocates 1–8 GiB of ephemeral storage by vCPU, with the note that it "disappears
when the container shuts down or restarts."

**Therefore the port's durable-state primitive is a network-attached transactional store, and nothing
else.** SQLite-on-a-volume is a compose-only optimisation, not the contract — which is exactly the
storage split [#10](https://github.com/jtmthf/kestrel/issues/10) already wrote into the rule of two
(SQLite default, Postgres second), now confirmed from the substrate side rather than assumed.

This is [#4's](https://github.com/jtmthf/kestrel/issues/4) ruling arriving a third time. Pause/resume
could not be in the compute contract; local disk cannot be in the substrate contract; both for the same
reason — **the capability splits down the middle of the required target set.**

### Realtime transport — five models, not four

[#10](https://github.com/jtmthf/kestrel/issues/10) named four. Reading the docs, there are five, because
Cloud Run and Container Apps are not the same as compose/k8s/ECS: the socket lives in *your* process, but
a platform-owned proxy kills it on a timer you may not be able to change, and the instance behind it can
vanish mid-connection.

| Model | Substrates | Who owns the socket | What kestrel must supply |
|---|---|---|---|
| **1. Long-lived server** | compose, Kubernetes, ECS | your process | cross-node bus (k8s, ECS); ALB idle timeout raised or heartbeats |
| **2. Proxy-capped server** | Cloud Run, Container Apps | your process, proxy-terminated | reconnect + cursor; external pub/sub; affinity is best-effort or absent |
| **3. Instance-pinned function** | Vercel | one function instance, for its life | external store for all state; reconnect lands elsewhere |
| **4. Gateway-held socket** | Lambda | API Gateway | `@connections` push, 32 KB frames, 128 KB messages, per-message stateless invoke |
| **5. Hibernatable actor** | Cloudflare | Durable Object | nothing — but "In-memory state is reset" on wake |

Model 5 is the one Open-Inspect gets for free and the one nothing else offers. Cloudflare's own framing
is the tell: hibernation exists so that "Billable Duration (GB-s) charges do not accrue during
hibernation" while clients stay connected. On every other substrate, an idle connected session either
costs a running instance or drops.

The load-bearing constraint is that **models 2, 3, and 4 all forbid connection-resident state**, and models
1 and 5 permit it. A contract that permits it is unimplementable on five of eight; a contract that forbids
it is implementable on all eight. So the port forbids it — which is precisely
[#10's](https://github.com/jtmthf/kestrel/issues/10) "no session state in the connection," now with the
substrate count attached.

### Scheduled wake-up — every timer is global, at-least-once, and imprecise

Not one scheduler surveyed offers a per-session timer except Cloudflare's, and Cloudflare's is capped at
one per object. Everything else is a global cron that must be multiplexed:

- **Kubernetes CronJob** — the controller creates "about one Job" per cycle; "in certain circumstances, a
  single CronJob **may be created twice, or none** at all." Miss more than **100 schedules** and the
  CronJob stops being reconciled.
- **EventBridge Scheduler** — "at-least-once event delivery," cron/rate/one-time, "flexible time windows"
  that deliberately disperse invocations. Scales to "millions of tasks."
- **Cloud Scheduler** — "designed to provide 'at least once' delivery," with exponential-backoff retries;
  targets are HTTP/S, Pub/Sub, App Engine.
- **Vercel Cron** — 100 per project. **Hobby is limited to once per day with "Per-hour (±59 min)"
  precision**; Pro and Enterprise get per-minute. A `0 1 * * *` job on Hobby "will trigger anywhere between
  1:00 am and 1:59 am."
- **Container Apps scheduled jobs** — standard five-field cron, "evaluated in Coordinated Universal Time."
- **Cloudflare** — Cron Triggers map an expression to a `scheduled()` handler; DO alarms are one per
  object, "guaranteed at-least-once execution," retried with "exponential backoff starting at a 2 second
  delay from the first failure with up to 6 retries allowed."

**Three properties are universal and belong in the port: at-least-once, no precision guarantee, and no
per-entity addressing.** The port therefore cannot be `schedule(at)` — it must be
`due(sessionId, notBefore)` written to the store, with the substrate's global tick draining whatever is
due. That inverts Open-Inspect's shape: the tick becomes an *adapter* concern and the due-time index
becomes kestrel's, so a substrate with one alarm slot and a substrate with one daily cron are both valid
implementations of the same port.

### Background execution — work may not outlive the request

This is where the substrates diverge most and where a naive port lies most convincingly.

| Substrate | Can work outlive the response? | Ceiling |
|---|---|---|
| compose / k8s / ECS | yes, indefinitely | operator's |
| Cloud Run | **only** with instance-based billing, which "allocates CPU even outside of request processing, letting you execute short-lived background tasks" | idle instance "can be shut down at any time"; not kept idle beyond 15 min |
| Container Apps | app: only with min replicas; job: `replicaTimeout` | jobs are the intended path — and **jobs have no ingress** |
| Vercel | `waitUntil`, but bounded by `maxDuration` | 300 s Hobby / 800 s Pro / 1800 s beta |
| Lambda | **no** — frozen after response | 900 s inside one invoke |
| Cloudflare | `waitUntil` ≈ 30 s past response | CPU 30 s default, 5 min max; alarms/queues 15 min wall |

Two structural notes. **Container Apps jobs cannot serve ingress** — the docs list "Ingress and related
features such as custom domains and SSL certificates" under "Jobs restrictions" — so on ACA the
long-running worker and the socket-serving app are necessarily *different deployments*. And **Cloud Run
jobs are the outlier in the other direction**: task timeout "up to 168 hours (7 days)," default 10
minutes, up to 10,000 tasks. The generous long-running primitive on every serverless substrate is the
job/task, never the request handler.

**Therefore the port cannot expose "run this in the background."** It must expose *durable, leased,
idempotent work items* that any later invocation can pick up. `waitUntil` becomes an advertised
accelerator — the same shape as [#4's](https://github.com/jtmthf/kestrel/issues/4) `snapshot` and
`pause_resume_memory` tiers — never a promise.

### Container images — the axis everyone assumes is wrong

**All eight targets accept a container image.** This was the least expected result of the survey.

- Lambda: "Container image code package size — 10 GB (maximum uncompressed image size, including all
  layers)."
- Vercel: add "a `Dockerfile.vercel` to your project root" and "Vercel builds the image, stores it in VCR,
  and serves it from a Function that scales automatically with traffic." Vercel Container Registry
  "supports the Docker Registry HTTP API v2, so `docker push`, `docker pull`, and `docker tag` work
  without new tooling."
- Cloudflare: Containers run images built from "`./Dockerfile`", on the Workers Paid plan, with instance
  types from `lite` (1/16 vCPU, 256 MiB, 2 GB disk) to `standard-4` (4 vCPU, 12 GiB, 20 GB disk), and
  "Total image storage per account" capped at 50 GB.

So **"does it take a container?" does not partition these eight, and it is the wrong question to build an
adapter taxonomy on.** The question that partitions them is *may a process outlive a request, and who
owns the socket* — which is the same axis the realtime table above found. One adapter taxonomy covers
both, which is a simplification worth taking.

Cloudflare is the interesting case: its container support is itself mediated by Durable Objects, and
`sleepAfter = "10m"` puts container lifetime under the same actor that owns the session. That is a very
good fit for kestrel's model and the only substrate where compute and session-actor are the same object.

### Default compute pairing — every substrate has a first-party answer

[#10](https://github.com/jtmthf/kestrel/issues/10) requires each adapter to name one.
[#4](https://github.com/jtmthf/kestrel/issues/4) worried that Fly and E2B being hosted-only made them
unusable as an on-ramp. **The stronger result: no adapter needs a third-party account at all.** Each
substrate's own platform ships an isolated-execution primitive.

| Substrate | Default pairing | Isolation | Notable |
|---|---|---|---|
| docker-compose | Docker daemon | container | [#4's](https://github.com/jtmthf/kestrel/issues/4) self-hostable spine |
| Kubernetes | Kubernetes Jobs | container/namespace | no pause/resume — pods are deleted |
| ECS | `RunTask` on Fargate | Firecracker (AWS-managed) | 20–200 GiB ephemeral per task |
| Cloud Run | Cloud Run jobs, second-generation | **microVM** — "provides full Linux compatibility rather than system call emulation" | 7-day task timeout |
| Container Apps | **dynamic sessions**, custom container pool | **Hyper-V isolation** | "Prewarmed pools enable subsecond launch times"; "allocated in milliseconds" |
| Vercel | Vercel Sandbox | **Firecracker microVM** | persistent sandboxes "auto-save state on stop and resume where you left off. Persistence is the default." |
| AWS Lambda | **Lambda MicroVMs** | microVM | "Maximum execution duration per MicroVM — 8 hours (28,800 seconds)"; `SuspendMicrovm`/`ResumeMicrovm` APIs; ARM64 only |
| Cloudflare | Containers via `@cloudflare/sandbox` | container in a DO-managed instance | **preview** — "Sandbox SDK 1.0 preview ... `@cloudflare/sandbox@next`" |

Two of these deserve flagging.

**Lambda MicroVMs change the Lambda story materially.** An 8-hour execution ceiling with explicit
suspend/resume APIs is a different animal from a 900-second function, and it lands squarely in the
territory [#4](https://github.com/jtmthf/kestrel/issues/4) surveyed for E2B and Fly. It is ARM64-only
("Lambda MicroVMs support the ARM64 (AWS Graviton) architecture") and its concurrency is quota-bound
(400 GB of memory across all MicroVMs by default). This was not in [#4's](https://github.com/jtmthf/kestrel/issues/4)
matrix and probably should be.

**Cloudflare's is the only pairing in preview.** That is corroborated by behaviour: Open-Inspect runs on
Cloudflare and pairs with *five external providers* — Daytona, E2B, Modal, OpenComputer, Vercel — rather
than anything first-party. On the substrate with the best control plane, the compute pairing is the
weakest.

---

## The narrowest port all eight satisfy

Six interfaces. Nothing in them names a mechanism, a cardinality, or a duration.

**1. `Store` — a transactional record store with compare-and-set.**
Not a filesystem. Not a local database. Reads and writes are network calls with a transaction boundary and
optimistic concurrency. Everything durable in kestrel lives here: sessions, transcripts, participants,
due-times, work items, leases.

**2. `Log` — an append-only per-session event log with monotonic sequence numbers.**
`append(sessionId, events) -> seq` and `read(sessionId, fromSeq, limit)`. This is the single mechanism
that makes a 240-second connection survivable and the one that
[#2](https://github.com/jtmthf/kestrel/issues/2) already demanded independently, opencode's stream being
"volatile by contract." It may be a table in `Store`; it is named separately because its access pattern
and retention rules differ.

**3. `Fanout` — best-effort, at-most-once notification.**
`publish(topic, hint)` and `subscribe(topic)`. **It carries no payload that matters** — only a nudge that
new sequence numbers exist. Correctness lives in `Log`; `Fanout` only makes reads timely. This is what
makes Redis pub/sub, `@connections`, a Durable Object, and a bare in-process emitter all valid
implementations, and it is why a dropped notification is a latency bug rather than a data-loss bug.

**4. `Timer` — `due(key, notBefore)` / `clear(key)`, at-least-once, no precision guarantee.**
The due-time index lives in `Store`. The adapter supplies a *tick* — a DO alarm, a Cron Trigger,
EventBridge Scheduler, a Kubernetes CronJob, a daily Vercel cron, or an in-process wheel — and the tick
drains what is due. The port never learns how many alarm slots exist.

**5. `Work` — durable, leased, idempotent work items.**
`enqueue(item)`, `claim(leaseMs) -> item`, `heartbeat`, `complete`, `fail`. Every unit of agent-adjacent
work is one of these. Nothing may assume it survives the invocation that created it. `waitUntil` and
long-lived workers are advertised accelerators that shorten the latency between enqueue and claim, never
a change in the guarantee.

**6. `Compute` — [#4's](https://github.com/jtmthf/kestrel/issues/4) contract, unchanged.**
Provision, exec + stream, read/write files, status, destroy — plus, new here, **the adapter must name its
default driver**, per [#10](https://github.com/jtmthf/kestrel/issues/10).

**What is deliberately absent, and why:**

- No connection identity. Models 2 and 3 cannot address a specific live socket from another instance.
- No local filesystem. Half the targets have none.
- No "run in background." Lambda freezes; Cloud Run throttles.
- No scheduling precision. Vercel Hobby is ±59 minutes.
- No cross-instance in-memory anything. Cloudflare resets it on wake; Vercel and Cloud Run scatter it.
- No pause/resume — already ruled out one layer down by [#4](https://github.com/jtmthf/kestrel/issues/4).

**The invariant that falls out of all six:** *a session's entire truth is in `Store` and `Log`; every
other component is a cache or a courier.* That is [#4's](https://github.com/jtmthf/kestrel/issues/4)
"reconstructible from control-plane state" and [#2's](https://github.com/jtmthf/kestrel/issues/2) "kestrel
needs its own durable event log" arriving as the same sentence.

## What each substrate must supply above the port

| Substrate | Store | Log | Fanout | Timer tick | Work runner |
|---|---|---|---|---|---|
| docker-compose | SQLite on a volume | same file | in-process emitter | in-process wheel | the same process |
| Kubernetes | Postgres | Postgres | Redis or NATS | `CronJob` (≥1 min, may double-fire) | Deployment or Job |
| ECS | RDS / Aurora | RDS | ElastiCache or SNS | EventBridge Scheduler | long-running service task |
| Cloud Run | Cloud SQL | Cloud SQL | Pub/Sub or Redis | Cloud Scheduler | Cloud Run job (7-day ceiling) |
| Container Apps | Azure Postgres | Azure Postgres | Azure Service Bus / Redis | scheduled job | **separate** job deployment (no ingress) |
| Vercel | Marketplace Postgres | Marketplace Postgres | Marketplace Redis | Vercel Cron (**Hobby: daily**) | function + Sandbox |
| AWS Lambda | DynamoDB / RDS | DynamoDB | API GW `@connections` + SQS | EventBridge Scheduler | Lambda, or a MicroVM for long work |
| Cloudflare | DO SQLite (10 GB/object) or D1 | DO SQLite | the DO itself | DO alarm + Cron Trigger | Container / Queue consumer (15 min) |

The compose column is the honest measure of the design: **on a single machine every one of these is the
same process and the same file**, which is what keeps `docker compose up` a real one-command path rather
than a compatibility shim.

## Where a v1 promise cannot be met

Findings, per the ticket's framing, not failures.

**1. Uniform multiplayer holds — but the number is 240 s, not 2 h.** The promise survives on all eight
because the port removes connection-resident state entirely. What does *not* survive is any design that
assumed a connection long enough to span a run. Reconnect-with-cursor is not a fallback path to test
occasionally; on Container Apps it is the *normal* path, several times per run.

**2. The one-command path fails on Vercel Hobby, and only there.** Two independent hard limits:
`maxDuration` is "300s default and maximum" with no extension, and cron is "limited to cron jobs that run
once per day" with the deployment *failing* if the expression would run more often — "Cron expressions
that would run more frequently will fail deployment." Since
[#10](https://github.com/jtmthf/kestrel/issues/10) made **schedule one of the five required round-trip
trigger sources**, a once-a-day timer with ±59 minutes of jitter does not satisfy that floor in any useful
sense. **Vercel's adapter should document a Pro plan as its floor**, in the same breath as vendor
credentials. This is the sharpest case of #10's rewording — *zero required kestrel config; the substrate's
cost of entry is the substrate's* — meeting a real bill.

**3. Container Apps needs two deployments, not one.** Jobs cannot serve ingress; apps that must not
scale to zero need `minReplicas ≥ 1`. A "one command" on ACA provisions an app *and* a job *and*
(if longer connections matter) a premium-ingress workload profile. It is still one command, but the
adapter is the most complex of the eight, and the cheapest configuration is the one with the tightest
realtime cap.

**4. Cloudflare's compute pairing is in preview.** Every other substrate's default pairing is GA. The
adapter can ship, but the pairing rule from [#10](https://github.com/jtmthf/kestrel/issues/10) is met with
a preview SDK, and that belongs in the release notes rather than being discovered.

**5. Lambda cannot host the control plane's long work in a function.** 900 seconds and a frozen
environment after response mean any run longer than 15 minutes must live in a MicroVM or a container. The
Lambda adapter is therefore structurally two-piece, like ACA's — which is a point in favour of the `Work`
port being the seam, since both substrates split at exactly that line.

**6. Nothing here re-opens [#4](https://github.com/jtmthf/kestrel/issues/4), but Lambda MicroVMs deserve a
pass.** 8-hour execution with suspend/resume is a compute backend
[#4](https://github.com/jtmthf/kestrel/issues/4) did not survey, and it is first-party to a required
substrate.

---

## Coverage gaps — not examined

Stated so they are not mistaken for coverage.

- **Container Apps `replicaTimeout` maximum.** The jobs docs and the quotas page read here document the
  setting ("the maximum time in seconds to wait for a replica to complete") but **state no upper bound**.
  Examples use 1800. Undetermined, and it decides whether ACA jobs can host long runs at all.
- **Lambda MicroVMs beyond the quota table.** The feature page returned no content; everything above comes
  from the quotas page. Suspend/resume *semantics* — whether memory survives, whether the filesystem
  persists, cold-start behaviour — is unread. This is the single largest gap.
- **Lambda durable functions.** The quotas page documents an entire durable-execution product (5M running
  executions per region, 3,000 durable operations per execution, 100 MB of checkpoint storage,
  `CheckpointDurableExecution`). Not examined. If it is what it appears to be, it overlaps the `Work` port
  directly. The same is true of **Vercel Workflows**, advertised on the limits page as letting "your code
  pause, resume, and maintain state for minutes to months without duration limits."
- **Cost.** Not modelled anywhere. Instance-based billing on Cloud Run, `minReplicas ≥ 1` on ACA, premium
  ingress workload profiles, and Cloudflare's Workers Paid requirement all convert a "serverless" adapter
  into a standing bill, and cheapness of adoption was the stated reason serverless was committed.
- **ALB WebSocket support** is universally reported but was not found stated in the ALB user-guide pages
  read; the idle-timeout numbers above are cited, the WebSocket claim is not.
- **Network Load Balancer, API Gateway HTTP APIs, AppRunner, Cloudflare Workers WebSocket outside a
  Durable Object**, and **SSE as an alternative to WebSocket on every substrate.** SSE in particular may
  have different proxy timeouts and is the obvious fallback transport; unexamined.
- **Cold-start figures**, other than Container Apps' "milliseconds" and Vercel Sandbox's "start in
  milliseconds," both vendor claims.
- **Secret injection and egress control per substrate** — flagged as a gap by
  [#4](https://github.com/jtmthf/kestrel/issues/4) too, still unexamined, still where governance lives.
- **Open-Inspect beyond `platform-ports.ts`, its Cloudflare adapters, and the alarm subsystem.** The
  ~36,700-line `session/` package was not read in this pass.
