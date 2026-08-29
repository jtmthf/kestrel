# Language and runtime: what actually constrains the choice

Extends [#2](https://github.com/jtmthf/kestrel/issues/2) (the opencode SDK audit) and
[#14](https://github.com/jtmthf/kestrel/issues/14) (the substrate-adapter capability matrix). Primary
docs and source read 2026-08-28. opencode source read at `sst/opencode@dev` (commit `df35e84`) and at
the published `@opencode-ai/client@0.0.0-beta-18600` npm tarball; the published beta OpenAPI document
read at `https://opencode.ai/v2/openapi.json`.

**What is inherited, not redone.** [#14](https://github.com/jtmthf/kestrel/issues/14) already
established that all eight targets accept a container image, that the connection ceiling is 240 s
(Azure Container Apps) rather than two hours, and that the axis that partitions the eight is *who owns
the socket*, not *does it take a container*. Everything in that document is taken as given here.
[#2](https://github.com/jtmthf/kestrel/issues/2) already established opencode's two embedding modes,
the volatility of `GET /api/event`, and the plugin hook surface.

**What is new here.** Q1 asks a question [#14](https://github.com/jtmthf/kestrel/issues/14) did not:
whether the *entry point* — the process that terminates HTTP — is language-constrained. Q2 goes to the
opencode source rather than the docs, and overturns two of
[#2's](https://github.com/jtmthf/kestrel/issues/2) findings: the authentication mechanism is now known
(HTTP Basic), and there **is** a durable, cursor-resumable per-session event log, which
[#2](https://github.com/jtmthf/kestrel/issues/2) left as an open question. Q3 is entirely new.

---

## Bottom line

**Seven of eight targets place no language constraint on the entry point.** Cloudflare is the sole
exception: every container request is "passed through a Worker," the Worker is code you write and
deploy, and the only documented API for controlling a container is JavaScript/TypeScript. AWS Lambda
is the near-miss — its entry point must speak the Lambda Runtime API rather than terminate HTTP, but
AWS ships that shim as a prebuilt binary (`aws-lambda-web-adapter`) you `COPY` into any image, so it
costs a Dockerfile line rather than a line of code in a particular language.

**opencode 2 can be driven from any language, and nothing load-bearing would have to be
reimplemented.** It is a standalone HTTP server (`opencode2 serve --hostname --port`) described by an
OpenAPI 3.1 document the running server publishes at `/openapi.json`, authenticated with HTTP Basic.
Both TypeScript clients are *generated* from that contract — one by `@hey-api/openapi-ts`, one by
opencode's own `httpapi-codegen` — and the hand-written residue is roughly two hundred lines of glue.
The in-process SDK is the same generated client wired to an in-memory `fetch`; its only same-process
addition is `tools.register`. Prompting, streaming, interrupting, switching models, approving a
permission and replying to a question are all plain HTTP or SSE. **The one genuinely TypeScript-only
surface is the plugin API**, and the two things kestrel would want from it have language-agnostic
substitutes: custom tools via MCP (`local` and `remote` servers are first-class in v2 config), and
approvals via `POST /api/session/:sessionID/permission/:requestID/reply`.

**The single-binary constraint does not eliminate anyone, but it ranks them.** Go and Rust produce
static binaries that run on `FROM scratch`. Bun cross-compiles to eight triples including musl and
embeds asset directories and SQLite, at the cost of a ~60 MB artifact and a base image carrying
`libgcc`/`libstdc++`. Deno cross-compiles "to all targets regardless of the host platform" but
publishes **no musl target**, so `deno compile` output cannot run on Alpine. Node's SEA is
`Stability: 1.1 - Active development`, is not CI-tested on Alpine or macOS x64, and loads native
addons only by writing them to a temp file and calling `process.dlopen()`.

The thing that actually binds: **Cloudflare forces a TypeScript Worker into the deployment regardless
of what language the control plane is written in.** That is one adapter's cost, not the project's.
Nothing else in the eight targets, and nothing in opencode 2, pushes toward TypeScript.

---

## Q1 — Does a container image still constrain the entry point's language?

### The table

| Target | Who terminates inbound HTTP/WS | Language constraint on the entry point | Shim required |
|---|---|---|---|
| **docker-compose** | your process | none | none |
| **Kubernetes** | your process | none | none |
| **ECS** (behind ALB) | your process | none | none |
| **Cloud Run** | your process, proxy-terminated | none; Linux x86_64 ABI only | none |
| **Azure Container Apps** | your process, proxy-terminated | none; `linux/amd64` only | none |
| **Vercel** | your process, in a Function | none | none — a rewrite is added automatically |
| **AWS Lambda** | **API Gateway** (WebSocket) / Lambda service (HTTP) | none, but the entry point must speak the Lambda Runtime API | **yes** — a Runtime Interface Client, or AWS's prebuilt `lambda-adapter` binary |
| **Cloudflare** | **a Worker → a Durable Object** → your container | **JavaScript/TypeScript** for the Worker | **yes** — a Worker you author |

**Count: seven of eight are language-agnostic at the entry point. One — Cloudflare — is not.** Of the
seven, six require no shim at all; Lambda requires a shim that AWS distributes as a compiled binary,
so it constrains the *protocol* rather than the language.

---

### Cloudflare — the Worker is not optional, and it is JavaScript

The lifecycle page states it without hedging:

> "Recall that Containers are backed by Durable Objects and Workers. Requests are first routed
> through a Worker …"
>
> "Because all Container requests are passed through a Worker, end-users cannot make non-HTTP TCP or
> UDP requests to a Container instance."
>
> — [Lifecycle of a Container](https://developers.cloudflare.com/containers/platform-details/)

The request path is Worker → Durable Object → container: "From the Worker, a request passes through a
Durable Object instance (the Container class extends a Durable Object class)"
([same page](https://developers.cloudflare.com/containers/platform-details/)).

**What the Worker owns.** Routing, and container lifecycle. It does *not* have to terminate the
WebSocket: the docs show a WebSocket forwarded straight through.

> "WebSocket requests are automatically forwarded to a container using the default `fetch` method on
> the Container class"
>
> ```js
> import { Container, getContainer } from "@cloudflare/containers";
> export class MyContainer extends Container {
>   defaultPort = 8080;
>   sleepAfter = "2m";
> }
> export default {
>   async fetch(request, env) {
>     return getContainer(env.MY_CONTAINER).fetch(request);
>   },
> };
> ```
>
> — [Websocket to Container](https://developers.cloudflare.com/containers/examples/websocket/)

So the container *can* hold the socket. **But that forecloses hibernation.** Hibernation is not a
property of the connection, it is a property of how the Durable Object accepted it: the Hibernation
WebSocket API requires the DO to call `state.acceptWebSocket(server)`, and the docs are explicit that
"Unlike `ws.accept()`, `state.acceptWebSocket(ws)` allows the Durable Object to be hibernated"
([Use WebSockets](https://developers.cloudflare.com/durable-objects/best-practices/websockets/)). A
socket proxied into a container is held by the DO as a live pipe, not accepted for hibernation.
*Inference, not a quoted claim: the docs do not state anywhere that a proxied-to-container WebSocket
hibernates, and the mechanism as documented cannot.* The choice is therefore **hibernation with the
DO terminating the socket, or the container terminating the socket with no hibernation** — you cannot
have both, and Open-Inspect's free lunch (noted in
[#14](https://github.com/jtmthf/kestrel/issues/14)) is only available on the first branch.

**What language the Worker must be.** Workers is documented as polyglot — "first-class support for
… JavaScript, TypeScript, Python Workers, Rust" plus WebAssembly for "C, C++, Kotlin, Go and more"
([Languages](https://developers.cloudflare.com/workers/languages/)). But Rust Workers are Wasm, not
native: the guide requires `rustup target add wasm32-unknown-unknown` and builds through
`worker-build` ([Rust](https://developers.cloudflare.com/workers/languages/rust/)). More decisively,
**the container-control surface is JavaScript only.** Every documented path goes through
`@cloudflare/containers` (an npm package) or `ctx.container` on a Durable Object, and the
`workers-rs` binding crate exposes no container type — its `worker-sys/src/types` directory lists
`durable_object.rs`, `websocket_pair.rs`, `d1.rs`, `queue.rs`, `r2.rs` and so on, with no `container`
entry ([`cloudflare/workers-rs`](https://github.com/cloudflare/workers-rs/tree/main/worker-sys/src/types),
read 2026-08-28). *Verified by directory listing; I found no first-party statement either permitting
or forbidding it.*

**Lifecycle and sleep.** `sleepAfter` defaults to `"10m"` and is described as "how long to keep the
container alive without activity before shutting it down," with `onActivityExpired()` "Called when
the `sleepAfter` timeout expires **with no incoming requests**" and a `renewActivityTimeout()` for
manual resets ([Container class](https://developers.cloudflare.com/containers/reference/container-class/)).
The existence of a manual renewal API is the tell: a long-held connection with no new *requests* is
not obviously activity, so a socket-holding container likely needs the Worker to renew the timer.
*The docs do not say whether an open WebSocket counts as activity — unsourced.* On shutdown the
platform "Sends `SIGTERM` to the main process in the container" and "Waits up to 15 minutes for that
process to exit"; "All disk is ephemeral. When a Container instance goes to sleep, the next time it is
started, it will have a fresh disk as defined by its container image"
([Lifecycle](https://developers.cloudflare.com/containers/platform-details/)). Instance types run
`lite` (1/16 vCPU, 256 MiB, 2 GB disk) through `standard-4` (4 vCPU, 12 GiB, 20 GB disk), with 50 GB
of total image storage per account
([Limits](https://developers.cloudflare.com/containers/platform-details/limits/)).

**Verdict: Cloudflare is the one target that puts TypeScript in the deployment no matter what the
control plane is written in.** The Worker is small — routing plus lifecycle — but it is authored code
in a language the platform picks.

---

### Vercel — a container image is a real deployable unit, and no shim is required

This is the finding that most changes the shape of the Vercel adapter. Vercel does not ask for a
JS/TS entry point:

> "Get started by creating a `Dockerfile.vercel` (or `Containerfile.vercel`) file placed at the root
> of your project. **Vercel automatically detects these files and adds a rewrite rule to route all
> traffic to the container image**"
>
> — [Container Images](https://vercel.com/docs/functions/container-images)

The same page's second example is `FROM nginx:alpine` copying static HTML — a deployment with no
JavaScript in it at all. The knowledge-base article is blunter:

> "Container images are how teams bring backend workloads to Vercel. That includes services written
> in Go, Rust, Python, or Ruby, custom servers, sidecar processes, and apps that depend on system
> libraries like FFmpeg. **If you can package it as an OCI image, you can deploy it on Vercel.**"
>
> — [Does Vercel support Docker deployments?](https://vercel.com/kb/guide/does-vercel-support-docker-deployments)

The contract is a port: "Vercel Functions running container images are expected to open an HTTP server
to receive traffic on. The default port is `80`, and it can be overridden by setting the `PORT`
environment variable." Scale-in is documented: "Functions not receiving any traffic for 5 minutes in
production environments, and 30 seconds in preview environments, will automatically scale down," with
`SIGTERM` and "a 30 seconds grace period"
([Container Images](https://vercel.com/docs/functions/container-images)).

**What Vercel admits for a long-lived process.** WebSockets are supported and require Fluid compute:

> "A single WebSocket connection is pinned to one Vercel Function instance. Messages sent over that
> connection reach the same function instance for the lifetime of the connection, and Fluid compute
> allows a single function instance to handle multiple WebSocket connections."
>
> "WebSocket connections close when a Vercel Function reaches its maximum duration."
>
> "WebSockets require Fluid compute to be enabled. This is the default for new projects created on or
> after April 23, 2025."
>
> — [WebSockets](https://vercel.com/docs/functions/websockets)

Max duration, with Fluid compute: Hobby 300 s default and maximum; Pro and Enterprise 300 s default,
800 s maximum, 1800 s "extended maximum," where the extended tier "is in beta … and are only
supported for specific Node.js, Bun, and Python runtime versions"
([Limitations](https://vercel.com/docs/functions/limitations)). That last clause matters: **a
container image is capped at 800 s, not 1800 s**, since the extended tier is enumerated by runtime and
containers are not in the list. The container-images page says "The same limits and Active CPU pricing
model of Vercel Functions apply to custom container images."

**The gap.** Every WebSocket example on the WebSockets page is a Node.js, Bun, or Python *runtime*
function; the container-images page never mentions WebSockets, and the WebSockets page never mentions
container images. **Whether a WebSocket upgrade is forwarded into a `Dockerfile.vercel` container is
unsourced.** This is the single most consequential unknown for the Vercel adapter, because it decides
whether the "one image everywhere" story survives on Vercel or whether Vercel needs a separate
runtime-function entry point for realtime.

---

### AWS Lambda — a protocol shim, not a language shim

**A container-image function is not a free-form image.** The requirement is explicit:

> "The container image must implement the Lambda runtime API for custom runtimes. The AWS open-source
> runtime interface clients implement the API. You can add a runtime interface client to your
> preferred base image to make it compatible with Lambda."
>
> "If you use an OS-only base image or an alternative base image, you must include a runtime interface
> client in your image. … AWS provides open-source runtime interface clients for the following
> languages: Node.js, Python, Java, .NET, Go, Ruby, Rust. If you're using a language that doesn't have
> an AWS-provided runtime interface client, you must create your own."
>
> — [Create a Lambda function using a container image](https://docs.aws.amazon.com/lambda/latest/dg/images-create.html)

So the entry point does not terminate HTTP; it polls the Runtime API for invocations. Go and Rust are
both covered by first-party RICs. **And there is a path that requires no code change in any language**
— AWS's own Lambda Web Adapter:

> "AWS Lambda Web Adapter allows developers to build web apps (http api) with familiar frameworks …
> and run it on AWS Lambda. The same docker image can run on AWS Lambda, Amazon EC2, AWS Fargate, and
> local computers."
>
> "Supports any web frameworks and languages, no new code dependency to include"
>
> ```dockerfile
> COPY --from=public.ecr.aws/awsguru/aws-lambda-adapter:1.0.1 /lambda-adapter /opt/extensions/lambda-adapter
> ```
>
> "Non-AWS base images may be used since the Runtime Interface Client ships with the Lambda Web
> Adapter."
>
> — [`awslabs/aws-lambda-web-adapter`](https://github.com/awslabs/aws-lambda-web-adapter)

One `COPY` line, a prebuilt multi-arch binary, and an ordinary HTTP server on `AWS_LWA_PORT`
(default 8080). **The Lambda constraint is therefore a protocol shim distributed as a binary, not a
language.**

**Duration and streaming.** "Code can run for up to 15 minutes in a single invocation"; the quota
table gives "Function timeout — 900 seconds (15 minutes)" and "Container image code package size —
10 GB (maximum uncompressed image size, including all layers)"
([Lambda quotas](https://docs.aws.amazon.com/lambda/latest/dg/gettingstarted-limits.html)). Response
streaming is runtime-scoped: "Lambda supports response streaming on Node.js managed runtimes. For
other languages, including Python, you can use a custom runtime with a custom Runtime API integration
to stream responses **or use the Lambda Web Adapter**," and bandwidth is capped — "The first 6 MB of
your function's response payload has uncapped bandwidth. After this initial burst, Lambda streams your
response at a maximum rate of 2 MBps"
([Response streaming](https://docs.aws.amazon.com/lambda/latest/dg/configuration-response-streaming.html)).

**WebSockets: Lambda never sees the socket.** API Gateway terminates it and invokes the function per
message.

> "In API Gateway you can create a WebSocket API as a stateful frontend for an AWS service (such as
> Lambda or DynamoDB) or for an HTTP endpoint. The WebSocket API invokes your backend based on the
> content of the messages it receives from client apps."
>
> "API Gateway calls the `$connect` route when a persistent connection between the client and a
> WebSocket API is being initiated. API Gateway calls the `$disconnect` route when the client or the
> server disconnects."
>
> "1001 — API Gateway returns this status code when the client is idle for 10 minutes or reaches the
> maximum 2 hour connection lifetime."
>
> "1003 — API Gateway returns this status code when an endpoint receives a binary media type.
> **Binary media types aren't supported for WebSocket APIs.**"
>
> — [Overview of WebSocket APIs](https://docs.aws.amazon.com/apigateway/latest/developerguide/apigateway-websocket-api-overview.html)

The binary-frame prohibition is new relative to [#14](https://github.com/jtmthf/kestrel/issues/14) and
is worth carrying: **on Lambda the realtime transport is text-only**, which rules out a compact binary
framing for the fan-out channel if kestrel wants one uniform wire format. Server-to-client pushes go
through "the `@connections` API to send a POST request" — i.e. an outbound HTTPS call from any
language, not a socket write.

---

### Cloud Run — stated in as many words

> "**Supported languages and images.** Your container image can run code written in the programming
> language of your choice and use any base image, provided that it respects the constraints listed in
> this page. Executables in the container image must be compiled for Linux 64-bit. Cloud Run
> specifically supports the Linux x86_64 ABI format."
>
> "The ingress container within an instance must listen for requests on `0.0.0.0` on the port to which
> requests are sent. Notably, the ingress container should not listen on `127.0.0.1`. By default,
> requests are sent to `8080` … Cloud Run injects the `PORT` environment variable into the ingress
> container."
>
> — [Container runtime contract](https://cloud.google.com/run/docs/container-contract)

The only *architecture* constraint worth noting for a multi-arch build: "If deploying a
multi-architecture image, the manifest list must include `linux/amd64`." Startup has a deadline —
"your instances must listen for requests within 4 minutes after being started."

Long-lived connections:

> "WebSockets applications are supported on Cloud Run with no additional configuration required.
> However, WebSockets streams are HTTP requests, which are still subject to the request timeout
> configured for your Cloud Run service …"
>
> "WebSockets requests are treated as long-running HTTP requests in Cloud Run. They are subject to
> request timeouts (currently up to 60 minutes and defaults to 5 minutes) even if your application
> server does not enforce any timeouts."
>
> "Though session affinity on Cloud Run provides best effort affinity, new WebSockets requests could
> still potentially connect to different instances, due to built-in load balancing."
>
> — [Using WebSockets](https://cloud.google.com/run/docs/triggering/websockets)

---

### Azure Container Apps — also stated in as many words

> "Containers in Azure Container Apps can use any runtime, programming language, or development stack
> of your choice."
>
> "Azure Container Apps supports: Any Linux-based x86-64 (`linux/amd64`) container image; Containers
> from any public or private container registry; Optional sidecar and init containers"
>
> — [Containers in Azure Container Apps](https://learn.microsoft.com/en-us/azure/container-apps/containers)

Ingress, unchanged from [#14](https://github.com/jtmthf/kestrel/issues/14): "Support for WebSocket and
gRPC" and "Request time out is 240 seconds"
([Ingress](https://learn.microsoft.com/en-us/azure/container-apps/ingress-overview)). ACA remains the
binding constraint on connection lifetime across all eight.

---

### ECS, Kubernetes, docker-compose — the trivial three

**ECS.** A task definition names "The Docker image to use with each container in your task"
([Task definitions](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/task_definitions.html));
nothing in it names a language. Behind an ALB, the WebSocket question that
[#14](https://github.com/jtmthf/kestrel/issues/14) flagged as *unsourced* is now closed:

> "**WebSockets.** Application Load Balancers provide native support for WebSockets. You can upgrade an
> existing HTTP/1.1 connection into a WebSocket (`ws` or `wss`) connection by using an HTTP connection
> upgrade. When you upgrade, the TCP connection used for requests (to the load balancer as well as to
> the target) becomes a persistent WebSocket connection between the client and the target through the
> load balancer. You can use WebSockets with both HTTP and HTTPS listeners. … Websockets are not
> supported for requests routed to target groups that have enabled target optimizer."
>
> — [Listeners for your Application Load Balancers](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/load-balancer-listeners.html)

The idle timeout stands as recorded: 60 s default, "valid range is 1 to 4000 seconds"
([Load balancer attributes](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/edit-load-balancer-attributes.html)).

**Kubernetes.** "A container image is a ready-to-run software package containing everything needed to
run an application: the code and any runtime it requires, application and system libraries, and
default values for any essential settings"
([Containers](https://kubernetes.io/docs/concepts/containers/)). Connection lifetime is the operator's
ingress, not the platform's.

**docker-compose.** A service is defined by `image:` or `build:`
([Compose file services](https://docs.docker.com/reference/compose-file/services/)). No timeout the
platform imposes, and the process outlives everything — which is exactly why
[#14](https://github.com/jtmthf/kestrel/issues/14) makes compose the honest measure of the design.

---

### The Q1 conclusion, precisely

- **Seven of eight targets place no language constraint on the process that terminates inbound HTTP.**
  compose, Kubernetes, ECS, Cloud Run, Azure Container Apps, Vercel and AWS Lambda all accept an
  arbitrary-language binary as the thing that serves traffic.
- **Six of eight require nothing in front of it at all.** Vercel adds its own rewrite; the other five
  route straight to your port.
- **AWS Lambda requires a shim, but not a language.** The entry point must speak the Lambda Runtime
  API. AWS publishes RICs for Node.js, Python, Java, .NET, Go, Ruby and Rust, and publishes
  `aws-lambda-web-adapter` as a prebuilt binary that ships the RIC and translates invocations into
  ordinary HTTP against your server. **The shim is one `COPY` line, and it is nobody's language.** The
  socket, separately, is never Lambda's: API Gateway holds it, text frames only, 10-minute idle and a
  2-hour ceiling.
- **Cloudflare is the exception.** A Worker is mandatory — "all Container requests are passed through
  a Worker" — the Worker is authored code, and the only documented API for starting, addressing and
  sleeping a container is the JavaScript `Container` class / `ctx.container`. **The required shim owns
  routing and container lifecycle, and must be written in JavaScript or TypeScript.** It may forward
  a WebSocket to the container, but doing so gives up Durable Object hibernation.

---

## Q2 — opencode 2's client surface, and whether driving it means sharing its process

### It is a server first, and the library is the server in a costume

**opencode 2 ships as a standalone binary that binds a real TCP port.** The CLI's `serve` handler
constructs a `node:http` server and listens:

```ts
// packages/cli/src/commands/handlers/serve.ts
function bind(hostname: string, port: number, password: string) {
  return Layer.build(
    HttpRouter.serve(createRoutes(password), { disableListenLog: true, disableLogger: true }).pipe(
      Layer.provideMerge(NodeHttpServer.layer(() => createServer(), { port, host: hostname })),
      ...
```

The command spec declares `--hostname` (default `127.0.0.1`), `--port` (auto-probing upward from
4096), and `--register`
([`packages/cli/src/commands/commands.ts`](https://github.com/sst/opencode/blob/dev/packages/cli/src/commands/commands.ts),
[`handlers/serve.ts`](https://github.com/sst/opencode/blob/dev/packages/cli/src/commands/handlers/serve.ts)).
Set `--hostname 0.0.0.0` and it is reachable from another container. **This is the first-class mode**,
and it is the mode kestrel wants, for the reason [#2](https://github.com/jtmthf/kestrel/issues/2)
already gave: the agent runs inside an isolated environment and the control plane sits outside it.

**The embedded mode is not a second implementation. It is the same generated client with `fetch`
replaced.** This is the most load-bearing single fact in the audit, and it is visible in twenty lines:

```ts
// packages/sdk-next/src/opencode.ts
const web = ... HttpRouter.toWebHandler(createEmbeddedRoutes()...)
const fetch = Object.assign(
  (input, init) => web.handler(new Request(input, init)),
  { preconnect: () => undefined },
) satisfies typeof globalThis.fetch
const client = yield* OpenCode.make({ baseUrl: "http://opencode.local" }).pipe(
  Effect.provide(FetchHttpClient.layer),
  Effect.provideService(FetchHttpClient.Fetch, fetch),
)
return { ...client, tools: { register: tools.register } }
```

([`packages/sdk-next/src/opencode.ts`](https://github.com/sst/opencode/blob/dev/packages/sdk-next/src/opencode.ts))

And `createEmbeddedRoutes()` differs from the network `createRoutes()` by exactly one thing — the auth
password is `Option.none()` instead of set:

```ts
// packages/server/src/routes.ts
export function createRoutes(password?: string) { return makeRoutes(password ? ...configLayer({ username: "opencode", password: Option.some(password) }) : ServerAuth.Config.layer) }
export function createEmbeddedRoutes() { return makeRoutes(ServerAuth.Config.configLayer({ username: "opencode", password: Option.none() })) }
```

([`packages/server/src/routes.ts`](https://github.com/sst/opencode/blob/dev/packages/server/src/routes.ts))

The `httpapi-codegen` README states the boundary as policy rather than accident:

> "Networked and embedded OpenCode use the same generated Effect client against network and in-memory
> `HttpClient` transports respectively; the embedded host structurally extends that client with
> same-process capabilities."
>
> — [`packages/httpapi-codegen/README.md`](https://github.com/sst/opencode/blob/dev/packages/httpapi-codegen/README.md)

**There is exactly one same-process capability, and it is `tools.register`.** The `sdk-next` README
confirms it: "It also exports `Tool` and exposes local-only `tools.register(...)`"
([`packages/sdk-next/README.md`](https://github.com/sst/opencode/blob/dev/packages/sdk-next/README.md)).

---

### The protocol: HTTP/REST plus SSE, with an OpenAPI document the server serves itself

The API is defined once as an Effect `HttpApi` and the server mounts it with an OpenAPI path:

```ts
return HttpApiBuilder.layer(Api, { openapiPath: "/openapi.json" }).pipe(...)
```

([`packages/server/src/routes.ts`](https://github.com/sst/opencode/blob/dev/packages/server/src/routes.ts))

The CLI's own `api` subcommand proves the loop closes — it fetches the live document and dispatches by
operation ID:

```ts
const response = await fetch(new URL("/openapi.json", transport.url), { headers: transport.headers })
```

([`packages/cli/src/commands/handlers/api.ts`](https://github.com/sst/opencode/blob/dev/packages/cli/src/commands/handlers/api.ts))

The published beta document is at
[`https://opencode.ai/v2/openapi.json`](https://opencode.ai/v2/openapi.json): **OpenAPI 3.1.0, 114
paths, 222 component schemas**, `info` reading `{"title": "opencode HttpApi", "version": "0.0.1",
"description": "Experimental HttpApi surface for selected instance routes."}` (read 2026-08-28). Every
group carries an "Experimental …" description in the source
([`packages/protocol/src/api.ts`](https://github.com/sst/opencode/blob/dev/packages/protocol/src/api.ts)),
which is the machine-readable form of the beta warning
[#2](https://github.com/jtmthf/kestrel/issues/2) already recorded.

**Transports in use:** ordinary JSON over HTTP for everything, `text/event-stream` for the two event
streams, and WebSocket for exactly two endpoints — `/api/pty/{ptyID}/connect` and
`/api/experimental/persistent-pty/{ptyID}/connect`, the only two operations in the published spec
carrying `x-websocket: true`. The client README says so directly: "Custom transports such as the PTY
WebSocket connection remain outside the generic HTTP client"
([`packages/client/README.md`](https://github.com/sst/opencode/blob/dev/packages/client/README.md)).
**There is no JSON-RPC, no gRPC, and no bidirectional protocol for the agent loop.**

**Authentication — this closes [#2's](https://github.com/jtmthf/kestrel/issues/2) open question.** It
is HTTP Basic, with an optional query-parameter carrier for browser WebSocket upgrades:

```ts
export function header(credentials?: Credentials) {
  const password = credentials?.password ?? process.env.OPENCODE_SERVER_PASSWORD
  if (!password) return undefined
  return `Basic ${Buffer.from(`${credentials?.username ?? process.env.OPENCODE_SERVER_USERNAME ?? "opencode"}:${password}`).toString("base64")}`
}
```

([`packages/server/src/auth.ts`](https://github.com/sst/opencode/blob/dev/packages/server/src/auth.ts))

The middleware accepts either `Authorization: Basic …` or an `auth_token` query parameter holding the
same base64 blob — with an inline comment explaining why: "Browsers cannot set headers on WebSocket
upgrades, so a ticketed PTY connect skips credential checks here"
([`packages/server/src/middleware/authorization.ts`](https://github.com/sst/opencode/blob/dev/packages/server/src/middleware/authorization.ts)).
**Auth is off when no password is configured**, which is why the embedded routes pass
`Option.none()`. Any HTTP client in any language can produce a Basic header.

---

### The event surface — and a correction to [#2](https://github.com/jtmthf/kestrel/issues/2)

There are **two** streams, and only one of them is volatile.

**1. `GET /api/event` — server-wide, volatile.** [#2](https://github.com/jtmthf/kestrel/issues/2) is
right about this one, and the source shows exactly why: the handler takes a bounded subscription with
`subscriberCapacity = 256`, emits a synthetic `server.connected` first, and merges a heartbeat every
15 seconds
([`packages/server/src/handlers/event.ts`](https://github.com/sst/opencode/blob/dev/packages/server/src/handlers/event.ts)).
A slow consumer overflows the bound. The published docs say it plainly: "Volatile by contract: a slow
consumer overflows and fails the stream, and events during disconnection are missed"
([api](https://opencode.ai/v2/docs/api/)).

**2. A durable, cursor-resumable per-session log.** This is new, and it answers
[#2's](https://github.com/jtmthf/kestrel/issues/2) open question *"Does
`GET /api/experimental/session/{sessionID}/log` offer replay from an offset — i.e. could it back a
durable log?"* — **yes.** The published beta spec gives `v2.session.log` an `after` cursor and a
`follow` flag, returning `text/event-stream`
([`https://opencode.ai/v2/openapi.json`](https://opencode.ai/v2/openapi.json), read 2026-08-28). On
the `dev` branch this has been promoted out of `/experimental` into two typed endpoints:

> `GET /api/session/:sessionID/history` — "Read one finite page of public durable Session events after
> an exclusive aggregate sequence. Newly committed events may appear on later pages."
>
> `GET /api/session/:sessionID/event?after=<seq>` — "Replay durable events after an aggregate
> sequence, then continue with new durable events."
>
> — [`packages/protocol/src/groups/session.ts`](https://github.com/sst/opencode/blob/dev/packages/protocol/src/groups/session.ts)

Events carry `durable: { aggregateID, seq, version }`
([`packages/protocol/src/groups/event.ts`](https://github.com/sst/opencode/blob/dev/packages/protocol/src/groups/event.ts)),
and the projector treats a missing sequence as a defect — `"Durable Session event is missing aggregate
sequence"`
([`packages/core/src/session/projector.ts`](https://github.com/sst/opencode/blob/dev/packages/core/src/session/projector.ts)).
Sessions and messages are persisted in SQLite through Drizzle
([`packages/core/src/session/sql.ts`](https://github.com/sst/opencode/blob/dev/packages/core/src/session/sql.ts)).

**The durable set deliberately excludes the token-level deltas.** Comparing `DurableDefinitions` with
`Definitions` in
[`packages/schema/src/session-event.ts`](https://github.com/sst/opencode/blob/dev/packages/schema/src/session-event.ts),
the four events present in the live stream but absent from the durable one are
`session.next.text.delta`, `session.next.reasoning.delta`, `session.next.tool.input.delta` and
`session.next.compaction.delta`. Everything else survives: `prompted`, `prompt.admitted`,
`step.started/ended/failed`, `text.started/ended`, `reasoning.started/ended`,
`tool.input.started/ended`, `tool.called/progress/success/failed`, `shell.started/ended`, `retried`,
`compaction.started/ended`, `agent.switched`, `model.switched`, `context.updated`, `synthetic`,
`moved`, and the three `revert.*` events.

**This does not retire kestrel's own log — it makes seeding it cheap.** kestrel still owns durability
across environment teardown (opencode's SQLite lives in the environment, which
[#4](https://github.com/jtmthf/kestrel/issues/4) established is disposable), and still owns
multiplayer. But the run-to-transcript bridge is now *replay from a cursor* rather than *hope you were
attached*, which is exactly the shape [#14's](https://github.com/jtmthf/kestrel/issues/14) `Log` port
wants. A run that reconnects mid-flight asks for `after=<last seq>` and loses nothing; smooth typing
is the only thing that is not replayable, which is the right thing to lose.

---

### Everything a control plane needs is an HTTP call

Read from
[`packages/protocol/src/groups/*.ts`](https://github.com/sst/opencode/tree/dev/packages/protocol/src/groups)
and cross-checked against the published spec:

| Need | Endpoint | Note |
|---|---|---|
| Create a session | `POST /api/session` | |
| Send a prompt | `POST /api/session/:id/prompt` | "Durably admit one session input and schedule agent-loop execution unless `resume` is false" |
| Stream progress | `GET /api/session/:id/event?after=` | durable replay + live |
| Page history | `GET /api/session/:id/history?after=&limit=` | |
| Wait for idle | `POST /api/session/:id/wait` | "Wait for a session agent loop to become idle" |
| Interrupt | `POST /api/session/:id/interrupt` | "Interrupt active execution owned by this OpenCode process" |
| **Approve/deny a permission** | `POST /api/session/:id/permission/:requestID/reply` | plus `GET /api/permission/request` to list pending |
| Raise a permission request | `POST /api/session/:id/permission` | "Evaluate and, when approval is required, create a permission request" |
| Answer a question | `POST /api/session/:id/question/:requestID/reply` / `…/reject` | |
| **Switch model per session** | `POST /api/session/:id/model` | "Switch the model used by subsequent provider turns" |
| Switch agent per session | `POST /api/session/:id/agent` | |
| List models / providers / agents / skills / commands | `GET /api/model`, `/api/provider`, `/api/agent`, `/api/skill`, `/api/command` | |

The `session.switchModel` endpoint settles the other open question
[#2](https://github.com/jtmthf/kestrel/issues/2) left: **per-session model selection is a documented
runtime operation, not a config-file-only affair.** kestrel can promise model choice per agent without
rewriting config on disk.

The permission reply endpoint settles the governance seam:
[#2](https://github.com/jtmthf/kestrel/issues/2) concluded that "the plugin hooks are the seam" for
answering `ask`. **They are not the only seam.** A remote, non-TypeScript control plane can list
pending requests and reply over HTTP, which is precisely the "route an approval outward to a human"
path the README's ninth capability requires.

---

### The TypeScript clients are generated, and the residue is glue

**`@opencode-ai/sdk` (published, `beta` tag `0.0.0-beta-18600`) is Hey API output.** Its build script
generates the OpenAPI document from the CLI, then runs the generator:

```ts
await $`bun dev generate > ${dir}/openapi.json`.cwd(opencode)
...
await createClient({
  input: "./openapi.json",
  output: { path: "./src/v2/gen", ... },
  plugins: [ { name: "@hey-api/typescript" }, { name: "@hey-api/sdk", instance: "OpencodeClient" }, { name: "@hey-api/client-fetch", baseUrl: "http://localhost:4096" } ],
})
```

([`packages/sdk/js/script/build.ts`](https://github.com/sst/opencode/blob/dev/packages/sdk/js/script/build.ts))

The rest of that file is three post-generation `String.replace` patches working around generator bugs.
The hand-written surface of the v2 SDK is four small files:
`v2/client.ts` (a `fetch` wrapper that disables timeouts, rewrites `x-opencode-directory` /
`x-opencode-workspace` headers into query parameters on GET/HEAD, and installs an error interceptor),
`v2/server.ts` (spawns `opencode serve` via `cross-spawn` and scrapes `opencode server listening on
<url>` from stdout), `error-interceptor.ts`, and `process.ts`
([`packages/sdk/js/src/v2/`](https://github.com/sst/opencode/tree/dev/packages/sdk/js/src/v2)).
**That is the whole of it.** A Go or Rust control plane reimplements a subprocess spawn and two header
rewrites, and gets the rest from any OpenAPI generator.

**`@opencode-ai/client` (published, description: "Private generation target for clients derived
directly from OpenCode's authoritative Effect `HttpApi`") is generated by opencode's own tool.** Its
build compiles the contract and emits two clients:

```ts
const contract = compile(ClientApi, { groupNames, endpointNames, omitEndpoints })
await Effect.runPromise(Effect.all([
  write(emitPromise(contract, ...), ".../src/generated"),
  write(emitEffectImported(contract, ...), ".../src/generated-effect"),
], ...))
```

([`packages/client/script/build.ts`](https://github.com/sst/opencode/blob/dev/packages/client/script/build.ts))

The generator's settled rules are explicit that the output is mechanical: "Generate a zero-Effect
Promise client with structural wire-oriented values, direct `fetch`, and syntax parsing without runtime
structural validation"; "Commit generated source for review; CI regenerates and fails when the worktree
changes"; and — the line that matters most for kestrel — "**Neither runtime reconnects
automatically**"
([`packages/httpapi-codegen/README.md`](https://github.com/sst/opencode/blob/dev/packages/httpapi-codegen/README.md)).
Inspecting the published tarball's `dist/promise/generated/client.js` confirms it: a `prepare` that
builds a `URL` and `Headers`, an `execute` that calls `fetch`, and a `request` that JSON-decodes or
throws a tagged `ClientError` (`@opencode-ai/client@0.0.0-beta-18600`, unpacked 801 KB, read
2026-08-28). **There is no domain logic in it.**

Only three endpoints are withheld from the generated client — `fs.read`, `pty.connect`,
`pty.connectToken`
([`packages/client/src/contract.ts`](https://github.com/sst/opencode/blob/dev/packages/client/src/contract.ts))
— and all three are raw-stream or WebSocket routes still reachable over plain HTTP.

---

### What ships, and which runtime it embeds

**opencode 2 ships as `@opencode-ai/cli@beta`, running a native binary named `opencode2`.** The v2 docs
say: "The package uses a trusted postinstall script to select the native `opencode2` binary for your
platform. It does not replace OpenCode 1's `opencode` binary, so you can keep both versions installed
and run them side by side" ([overview](https://opencode.ai/v2/docs/)).

**The binary is Bun.** The build configuration is unambiguous — `Bun.build({ ..., compile: { target:
"bun-linux-x64-musl", outfile: ..., execArgv: [...] } })` across twelve targets (linux/darwin/win32 ×
x64/arm64 × musl/glibc × baseline)
([`packages/cli/script/build.ts`](https://github.com/sst/opencode/blob/dev/packages/cli/script/build.ts);
the v1 equivalent is
[`packages/opencode/script/build.ts`](https://github.com/sst/opencode/blob/dev/packages/opencode/script/build.ts)).
The repo pins `"packageManager": "bun@1.3.14"`
([`package.json`](https://github.com/sst/opencode/blob/dev/package.json)). The official container
image is Alpine with three packages:

```dockerfile
FROM alpine AS base
RUN apk add libgcc libstdc++ ripgrep
COPY dist/opencode-linux-x64-baseline-musl/bin/opencode /usr/local/bin/opencode
ENTRYPOINT ["opencode"]
```

([`packages/opencode/Dockerfile`](https://github.com/sst/opencode/blob/dev/packages/opencode/Dockerfile))

**kestrel does not need to be Bun to run opencode.** Both facts are convenient rather than binding: the
binary is self-contained, it takes `serve --hostname --port`, and the container image above is 20 MB of
Alpine plus one executable.

---

### Local discovery: a JSON file, and it is documented

The `@opencode-ai/client/service` module manages a background server. Its comment states the contract:

> "The registration file is the complete discovery contract. This module is intentionally implemented
> with Node APIs so Promise clients do not need Effect or `@effect/platform-node` at runtime."

The file lives at `$XDG_STATE_HOME/opencode/service.json`, falling back to
`~/.local/state/opencode/service.json`, and holds
`{ id?, version?, url, pid, password? }`; `headers()` turns that into
`Basic base64(username:password)` (`@opencode-ai/client@0.0.0-beta-18600`,
`dist/promise/service.js` and `dist/service.d.ts`, read 2026-08-28). The v1 CLI writes the equivalent
at `<global>/server.json` alongside a `password` file
([`packages/cli/src/services/daemon.ts`](https://github.com/sst/opencode/blob/dev/packages/cli/src/services/daemon.ts)).

**This is the only part of the client surface that touches the filesystem, and it is a convenience.**
A control plane that provisions the environment already knows the URL and the password — it can set
`OPENCODE_SERVER_PASSWORD` itself
([`packages/server/src/auth.ts`](https://github.com/sst/opencode/blob/dev/packages/server/src/auth.ts))
— so discovery never arises.

---

### Q2's answer

**Yes. A control plane in Go, Rust, or anything else can drive opencode 2 without reimplementing any
part of it.** The evidence, ranked by weight:

1. The embedded SDK *is* the network client with an in-memory `fetch`, and the embedded route set is
   the network route set with auth disabled. There is no privileged in-process API surface to be
   locked out of.
2. The API is one OpenAPI 3.1 document, served by the running process at `/openapi.json` and published
   at `opencode.ai/v2/openapi.json`. Both TypeScript clients are code-generated from it; one of them by
   an off-the-shelf generator any language has an equivalent of.
3. Auth is HTTP Basic.
4. Governance, model choice, interrupt, and durable replay are all HTTP or SSE.

**What would have to be written rather than reimplemented, precisely:**

- **A plugin, if and only if kestrel needs a hook the HTTP surface does not expose.** Plugins are npm
  packages loaded into the opencode process, declared in config as
  `{ package: string, options?: Record<string, unknown> }`
  ([`packages/core/src/config/plugin.ts`](https://github.com/sst/opencode/blob/dev/packages/core/src/config/plugin.ts)),
  and typed against `@opencode-ai/plugin`
  ([`packages/plugin/src/index.ts`](https://github.com/sst/opencode/blob/dev/packages/plugin/src/index.ts)).
  The host wires `agent`, `aisdk`, `catalog`, `command`, `credential`, `integration`, `model`,
  `provider`, `reference` and `skill` capabilities into a plugin
  ([`packages/core/src/plugin/host.ts`](https://github.com/sst/opencode/blob/dev/packages/core/src/plugin/host.ts)).
  **This is real TypeScript in opencode's process, and there is no network equivalent for it.**
  The two things [#2](https://github.com/jtmthf/kestrel/issues/2) wanted from plugins have
  substitutes, though:
  - *Custom tools* → **MCP**, which v2 config supports as `local` (a command, cwd, env, timeouts) or
    `remote` (a URL, headers, OAuth)
    ([`packages/core/src/config/mcp.ts`](https://github.com/sst/opencode/blob/dev/packages/core/src/config/mcp.ts)).
    A tool server written in any language, addressed over a documented protocol.
  - *Approvals* → the permission endpoints above.
  What remains plugin-only is *interception*: rewriting a tool's input before it runs, mutating a
  shell command, or amending the model request. If kestrel's policy enforcement needs that rather than
  approve/deny, it needs a small TypeScript plugin loaded into the environment — **a build artifact
  shipped with the runtime image, not a language choice for the control plane.**
- **Config injection**, which is already language-agnostic: `OPENCODE_CONFIG_CONTENT` takes the whole
  config as a JSON string
  ([`packages/sdk/js/src/v2/server.ts`](https://github.com/sst/opencode/blob/dev/packages/sdk/js/src/v2/server.ts)).
- **An SSE client with reconnect-and-resume.** Not a reimplementation — the generated TypeScript
  clients do not do this either ("Neither runtime reconnects automatically"), and
  [#14](https://github.com/jtmthf/kestrel/issues/14) already put reconnect-with-cursor on kestrel's
  side of the line.

**The standing risk is unchanged and worth restating.** The API document describes itself as
"Experimental HttpApi surface for selected instance routes" at version `0.0.0`, every group is titled
"Experimental", and the migration guide says "OpenCode 2.0 is in beta. Features may break
unintentionally, and the server and plugin APIs may continue to change"
([migrate-v1](https://opencode.ai/v2/docs/migrate-v1/)). Being generated from a machine-readable spec
makes churn *tractable* — regenerate and see what broke — not *absent*.

---

## Q3 — Single-binary and single-image distribution today

The constraint: one command to a working system, zero required config, docker-compose on the user's own
machine as the free on-ramp.

| | How a single executable is produced | Cross-compilation | Static assets | SQLite | Native addons / FFI | Base image |
|---|---|---|---|---|---|---|
| **Go** | `go build` | `GOOS`/`GOARCH`; no toolchain needed at `CGO_ENABLED=0` | `//go:embed` (stdlib) | pure-Go `modernc.org/sqlite`, or cgo `mattn/go-sqlite3` | cgo — breaks the static build | `scratch` (at `CGO_ENABLED=0`) |
| **Rust** | `cargo build --release` | `rustup target add` **plus a linker for the target** | `include_bytes!` / `include_str!` (std) | `rusqlite` with `bundled` compiles SQLite from source | native by definition | `scratch` on `*-linux-musl` |
| **Bun** | `bun build --compile` | 8 targets incl. **musl**, from any host | `--asset <dir>`, `with { type: "file" }` | `bun:sqlite` in the runtime | `.node` files embed | Alpine + `libgcc`, `libstdc++` |
| **Deno** | `deno compile` | "all targets regardless of the host platform" — **6 targets, gnu only, no musl** | `--include`, `--include-as-is` | `node:sqlite` | embeds the `node_modules` tree | glibc (Debian slim); **not Alpine** |
| **Node** | `node --build-sea config.json` | possible, but supply the target `node` and disable snapshot/code cache | `assets` map + `sea.getAsset()` | `node:sqlite`, Stability 1.2 RC | temp-file + `process.dlopen()` | glibc; **not CI-tested on Alpine** |

### Bun

`bun build --compile` with `--target`; the supported list is `bun-linux-x64`, `bun-linux-arm64`,
`bun-windows-x64`, `bun-windows-arm64`, `bun-darwin-x64`, `bun-darwin-arm64`, `bun-linux-x64-musl`,
`bun-linux-arm64-musl`, and "The segments of the `--target` value can appear in any order"
([Standalone executables](https://bun.com/docs/bundler/executables)). Assets: `--asset` embeds "a file
or directory tree into the executable under its original relative path. The embedded files live under
`import.meta.dir` at runtime and are reachable via `node:fs` … and `Bun.file()`"; individual files use
`with { type: "file" }`. SQLite: "You can use `bun:sqlite` imports with `bun build --compile`" — note
that an *embedded* `.db` is memory-backed and "all changes are lost when the executable exits," so a
persistent database must be an ordinary runtime path. Native addons: "You can embed `.node` files into
executables." Startup: `--bytecode` "moves that parsing work from runtime to bundle time."

**Size, measured rather than claimed.** The Bun docs give no figure. opencode's own release —
a Bun-compiled binary with the whole agent and an embedded web UI — publishes
`opencode-linux-x64.tar.gz` at 60,534,407 bytes and `opencode-linux-x64-musl.tar.gz` at 62,902,634
bytes (`sst/opencode` release `v1.18.25`, GitHub Releases API, read 2026-08-28). **~60 MB compressed
is the realistic order of magnitude**, and the musl binary still needs `libgcc` and `libstdc++`
([opencode's Dockerfile](https://github.com/sst/opencode/blob/dev/packages/opencode/Dockerfile)), so
`FROM scratch` is out.

### Deno

`deno compile`. "Deno supports cross compiling to all targets regardless of the host platform" — and
the table lists six: `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`, `x86_64-apple-darwin`,
`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
([`deno compile`](https://docs.deno.com/runtime/reference/cli/compile/)). **There is no musl target.**
That is the sharpest single constraint in this table for a project whose on-ramp is a small container
image: a `deno compile` artifact cannot run on Alpine.

Mechanically it is otherwise strong. "`deno compile` embeds your program into `denort` … a stripped
build of Deno that contains only what's needed to run a compiled program"; cross-compilation "fetches
that platform's `denort`" and caches it, after which builds "work offline." Assets: `--include`
embeds files or directories readable via `import.meta.dirname`, and `--include-as-is` embeds them
"without any module resolution … the right choice for pre-built frontend bundles (for example Vite or
webpack output)." Dependencies: "By default, `deno compile` embeds the entire resolved `node_modules`
tree in the executable," and with the experimental `--bundle`, "When a CommonJS package or a native
addon (`.node`) is reached, the relevant packages are embedded so they keep working at runtime."
A compiled binary also gets real persistent storage: "`Deno.openKv()` called without a path opens a
persistent database there instead of falling back to an in-memory one," keyed off `--app-name`.
SQLite arrives via `node:sqlite` ([Deno `node:sqlite`](https://docs.deno.com/api/node/sqlite/)).

### Node

Single executable applications are **`Stability: 1.1 - Active development`**, with `--build-sea` added
in v25.5.0 ([Single executable applications](https://nodejs.org/api/single-executable-applications.html)).
It "supports running a single embedded script," so the app must be bundled to one file first. Assets
go in an `assets` map and come back through `sea.getAsset()` / `getAssetAsBlob()` / `getRawAsset()`.

Three constraints matter:

- **Cross-compilation is awkward.** "The version of the Node.js binary used to produce the blob must be
  the same as the one to which the blob will be injected," and "When generating cross-platform SEAs
  (e.g., generating a SEA for `linux-x64` on `darwin-arm64`), `useCodeCache` and `useSnapshot` must be
  set to `false` to avoid generating incompatible executables."
- **Native addons are a dance.** "Native addons can be bundled as assets … The addon can then be
  loaded in the injected main script by writing the asset to a temporary file and loading it with
  `process.dlopen()`." There is even a documented arm64-container caveat: "if the single-executable
  application is produced by postject running on a Linux arm64 docker container, the produced ELF
  binary does not have the correct hash table to load the addons and will crash on
  `process.dlopen()`."
- **Alpine is not tested.** "Single-executable support is tested regularly on CI only on the following
  platforms: Windows; macOS (arm64 only; x64 is not currently supported and is skipped in the tests);
  Linux (all distributions supported by Node.js **except Alpine** and all architectures supported by
  Node.js except s390x)."

The native-addon problem is avoidable for the one case that matters: `node:sqlite` is built in and is
now `Stability: 1.2 - Release candidate` (added v22.5.0; "v25.7.0 — SQLite is now a release
candidate") ([`node:sqlite`](https://nodejs.org/api/sqlite.html)).

### Go

`go build` produces one executable; cross-compilation is `GOOS`/`GOARCH` environment variables, with
`CGO_ENABLED` documented as "Whether the cgo command is supported. Either 0 or 1"
([`cmd/go`](https://pkg.go.dev/cmd/go)). Assets are first-class in the standard library: "Go source
files that import `embed` can use the `//go:embed` directive to initialize a variable of type string,
`[]byte`, or `FS` with the contents of files read from the package directory or subdirectories at
compile time" ([`embed`](https://pkg.go.dev/embed)).

**SQLite is the one decision.** The conventional driver requires cgo — "`go-sqlite3` is *cgo* package.
If you want to build your app using go-sqlite3, you need gcc … you are required to set the environment
variable `CGO_ENABLED=1` and have a `gcc` compiler present within your path"
([`mattn/go-sqlite3`](https://github.com/mattn/go-sqlite3)) — which surrenders both the trivial
cross-compile and the `scratch` image. The alternative preserves both: `modernc.org/sqlite` is "a
sql/database driver using a **CGo-free port** of the C SQLite3 library"
([`modernc.org/sqlite`](https://pkg.go.dev/modernc.org/sqlite)), with per-`GOOS`/`GOARCH` support
tables. It carries its own caveat, stated by the package: "When you import this package you should use
in your go.mod file the exact same version of `modernc.org/libc` as seen in the go.mod file of this
repository."

With `CGO_ENABLED=0` the artifact runs on the "reserved, minimal `scratch` image"
([Create a base image](https://docs.docker.com/build/building/base-images/)).

### Rust

`cargo build --release` produces "a single distributable binary" — `--crate-type=bin` "will link in all
Rust and native dependencies" ([Linkage](https://doc.rust-lang.org/reference/linkage.html)). Assets go
in with the `include_bytes!` / `include_str!` standard macros.

**Cross-compilation is the weak point, and the docs say so.** "`rustup target add` only installs the
Rust standard library for a given target. There are typically other tools necessary to cross-compile,
particularly a linker"
([rustup — Cross-compilation](https://rust-lang.github.io/rustup/cross-compilation.html)). That is a
per-target C toolchain in CI, not an environment variable.

**Static linking is a default on musl.** "All targets in the compiler have a default mode of linking to
the C runtime. Typically targets are linked dynamically by default, but there are exceptions which are
static by default such as: … `x86_64-unknown-linux-musl`"
([Linkage](https://doc.rust-lang.org/reference/linkage.html)), and both
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` are **Tier 2 with Host Tools**
("guaranteed to build," full standard library)
([Platform support](https://doc.rust-lang.org/rustc/platform-support.html)). A musl build runs on
`scratch`.

**SQLite embeds cleanly, at the cost of a C compiler in the build.** `rusqlite`'s `bundled` feature
"causes us to automatically compile and link in an up to date version of SQLite for you. This avoids
many common build issues, and avoids depending on the version of SQLite on the users system"; under
the hood "`libsqlite3-sys` will use the `cc` crate to compile SQLite or SQLCipher from source and link
against that. This source is embedded in the `libsqlite3-sys` crate"
([`rusqlite`](https://github.com/rusqlite/rusqlite)). Note the interaction: **`bundled` plus
cross-compilation means a cross C compiler**, which is the same tax Go pays for `mattn/go-sqlite3`.

### What Q3 actually decides

Nothing is disqualified. The ordering that falls out:

- **Go and Rust** give the smallest image (`scratch`) and the cleanest cross-compile — Go decisively so
  at `CGO_ENABLED=0` with a pure-Go SQLite; Rust needs a per-target toolchain but rewards it with a
  fully static musl binary.
- **Bun** is the only JavaScript runtime that cross-compiles to musl, embeds asset trees and native
  addons, and has SQLite in the runtime. It is also the only one with a working existence proof at
  this exact scale — opencode itself. The price is a ~60 MB artifact and an Alpine base rather than
  `scratch`.
- **Deno's** missing musl target is disqualifying for a project whose free on-ramp is a small
  container image, unless the image is Debian-based.
- **Node's SEA** is the weakest of the five for this purpose: an experimental feature, untested on
  Alpine, that requires bundling first and dlopen-from-tmpfile for anything native.

---

## Coverage gaps — not examined

Stated so they are not mistaken for coverage.

- **Whether a Vercel `Dockerfile.vercel` container receives WebSocket upgrades.** The WebSockets page
  documents Node.js, Bun and Python *runtimes*; the container-images page never mentions WebSockets.
  This is the largest single gap in Q1 and it decides whether Vercel can run one image or needs two
  entry points.
- **Whether an open WebSocket counts as "activity" for Cloudflare's `sleepAfter` timer.** The docs say
  the hook fires "with no incoming requests" and provide `renewActivityTimeout()`, which implies but
  does not state the answer.
- **Whether a Python Worker can call `ctx.container`.** Verified only that `workers-rs` has no
  container type. Python Workers were not examined.
- **Cloudflare Workers holding a WebSocket outside a Durable Object**, and SSE as an alternative
  transport on every substrate — both carried over unexamined from
  [#14](https://github.com/jtmthf/kestrel/issues/14). SSE is the more urgent of the two now that
  opencode's own durable stream is SSE.
- **The exact shape of `SessionLogItemEncoded`** in the published beta spec: it is declared as
  `{"type": "string", "contentMediaType": "application/json"}` — an opaque JSON string. The typed
  `SessionEvent.Durable` union exists only on the `dev` branch. Whether the shipping beta's log items
  match that union was not verified.
- **The divergence between `sst/opencode@dev` and the shipping beta.** The published spec has 114
  paths including `/api/session/{id}/fork`, `/inbox`, `/form`, `/background`, `/generate`, `/export`;
  the `dev` branch's committed `openapi.json` has 51 `/api/` paths and a different session group. Both
  are opencode 2; they are not the same build. Any concrete integration should generate against the
  `/openapi.json` of the pinned binary rather than either of these.
- **opencode plugin loading mechanics** — how a `package` specifier resolves (npm, `file://`, workspace)
  was not traced to the loader on the v2 path.
- **Startup and cold-start figures** for any of the five Q3 runtimes. No first-party numbers were
  found and none are asserted here.
- **Binary sizes for Go, Rust, Deno and Node SEA.** Only Bun has a measured datapoint, and it comes
  from opencode's releases rather than from a controlled comparison.
- **Lambda Managed Instances**, referenced by the Lambda Web Adapter's feature list, and **Lambda
  MicroVMs**, flagged by [#14](https://github.com/jtmthf/kestrel/issues/14) as its largest gap. Still
  unexamined.
