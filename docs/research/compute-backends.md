# Pluggable compute backends for isolated execution

Resolves [#4](https://github.com/jtmthf/kestrel/issues/4). Primary docs read 2026-08-26.

## The finding: the minimal contract cannot include pause/resume

kestrel's hard requirement is that it runs on **Kubernetes and docker-compose**. Both are structurally
incapable of pausing a workload and resuming it later with memory state intact.

Kubernetes is explicit — the Job docs state:

> "Suspending a Job will delete its active Pods until the Job is resumed again."

Pods are **deleted**, not paused; resuming creates new pods from scratch. Docker's `docker pause` only
freezes a cgroup on the same host and does not survive the daemon or the machine; CRIU-based checkpoint
remains experimental.

Meanwhile microVM and sandbox backends have excellent pause/resume. So the capability is **split down the
middle of the required backend set**.

**Therefore: durable sessions cannot be delegated to the compute layer.** Snapshot/resume is an
*optional accelerating capability*, never a guarantee kestrel can make on every backend. A kestrel
session must be reconstructible from control-plane state on a cold, empty environment.

This converges with [#2](https://github.com/jtmthf/kestrel/issues/2), which found opencode's event stream
lossy and its snapshots non-portable. **Two independent investigations arrive at the same requirement:
kestrel owns durable session state in its control plane, and everything below is replaceable.**

## Capability matrix

| Backend | Isolation | Cold start | Pause/resume **with memory** | Snapshot / fork | Cross-host restore | Local self-host |
|---|---|---|---|---|---|---|
| **Firecracker** | microVM (KVM) | "Boot in <125ms", "<5 MiB overhead per VM", "up to 150 microVMs per second per host" | **Yes** — `PATCH /vm {"state":"Paused"}` → `PUT /snapshot/create` → `"Resumed"` | Full + Diff (diff is **developer preview**) | **Limited** | Yes |
| **Fly Machines** | microVM | "started and stopped at subsecond speeds" | **Yes** — `suspended` state | Not documented on pages read | n/a (managed) | **No** — hosted only |
| **E2B** | microVM sandbox | not stated | **Yes** — `sbx.pause()` / `sbx.connect()` | **Yes** — snapshots, forking, filesystem-only snapshots | n/a (managed) | Not documented; API-key hosted |
| **gVisor** | user-space kernel | not stated | **Yes** — checkpoint/restore | via checkpoint files | Possible with CPU-feature annotations; external state cannot migrate | Yes |
| **Kubernetes Jobs** | container/namespace | pod scheduling (seconds) | **No** — pods are deleted | No | No | Yes |
| **Docker / compose** | container | sub-second | **No** (host-local freeze only; CRIU experimental) | `commit` = filesystem only | No | Yes |
| **Kata Containers** | microVM | **not examined** | **undetermined** | undetermined | undetermined | Yes |

### Detail worth carrying forward

**Firecracker** — the richest self-hostable option. Snapshots capture guest memory, emulated hardware
state, and device model state; disk backing files are user-managed and *not* part of the snapshot.
Loading requires disk files, network TAPs and vsock sockets at the same relative paths, and the restored
microVM comes back **Paused** and must be explicitly resumed. Cross-host restore is real but narrow:
"Snapshots can be resumed on identical hardware instances where they were taken on, but using newer host
kernel versions." Caveats: high restore latency under cgroups v1, **network connectivity is not
guaranteed after resume**, and resuming a snapshot taken during early guest boot "might lead to crashes."

**E2B** — the most complete durable-session story of anything surveyed. Pause preserves "the sandbox's
filesystem and memory state" including "all the running processes, loaded variables, data." Pause costs
"approximately 4 seconds per 1 GiB of RAM"; resume is "approximately 1 second"; paused sandboxes persist
**"indefinitely"** with no auto-kill, requiring explicit `.kill()`. Continuous runtime is capped (24h Pro,
1h Hobby) before forced pause/kill, and `onTimeout: 'pause'` automates that. It also ships sandbox
**forking** — "Snapshot a running sandbox and boot new sandboxes from that exact state in a single call"
— which is a natural fit for agent swarms.

**gVisor** — checkpoint/restore works, but network state does not survive: with `--network=host` "host
sockets cannot be saved," backlogged connections are lost, and "sockets that were connected at checkpoint
time return `ECONNRESET`." Anything resumed must reconnect.

**A cross-cutting pattern:** every backend that *can* resume warns that **network connections do not
survive it**. Any long-lived agent session must be able to re-establish its connections after a resume
regardless of backend. That is a kestrel-level design requirement, not a backend detail.

## Proposed minimal contract

Operations every surveyed backend can honestly support:

1. **Provision** an environment from an image/template spec
2. **Exec** a command; **stream** stdout/stderr
3. **Read/write files** in the workspace
4. **Report status** / liveness
5. **Destroy**

Everything else is a **capability tier** a driver advertises rather than a promise kestrel makes:

- `snapshot` — Firecracker, E2B, gVisor
- `pause_resume_memory` — Firecracker, Fly, E2B, gVisor
- `fork` — E2B (Firecracker diff snapshots, with caveats)
- `cross_host_restore` — Firecracker (narrow), gVisor (with annotations)

Consequences worth stating plainly in the north star: kestrel's promises hold on the **floor**, and
richer backends make sessions cheaper and faster rather than more capable. Fly and E2B are **hosted
only**, so they cannot be the on-ramp; Firecracker plus Docker/Kubernetes is the self-hostable spine.

## Coverage gaps — not examined

Honest limits of this survey. Each would need its own pass before a driver decision:

- **Kata Containers** — not examined at all.
- **Modal, Daytona, Vercel Sandbox, OpenComputer** — the providers Open-Inspect actually ships against
  ([#5](https://github.com/jtmthf/kestrel/issues/5)). Given it is the closest analogue, its provider set
  is a strong signal about what works in practice.
- **Cold-start figures for E2B, gVisor, Kata** — not published on the pages read. Firecracker's `<125ms`
  and Fly's "subsecond" are vendor claims, not independent benchmarks.
- **Secret injection and network egress control** — thin across all backends on the pages read; deserves
  a dedicated pass, since it is where the governance story lives.
