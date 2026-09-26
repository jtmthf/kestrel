# Browser streaming against the operator boundary

Research gathered 26 September 2026 for
[Research browser streaming against the operator boundary](https://github.com/openkestrel/kestrel/issues/298).
It asks what constrains a browser Client that follows the operator boundary over SSE
([ADR-0015](../adr/0015-the-cli-is-a-client-not-a-role.md)), and how TanStack Start and TanStack
Query fit it. Sources are the WHATWG HTML and Fetch standards, MDN, browser engine source, RFC 9113,
TanStack's documentation and source at pinned commits, the Rust crates kestrel locks, and published
security guidance. It reports facts and marks every inference as one. It does not pick a delivery
mechanism or a stack shape: those belong to
[Decide how the Client learns that Sessions, Runs and the queue changed](https://github.com/openkestrel/kestrel/issues/310)
and
[Decide how the browser Client is built, served and kept same-origin](https://github.com/openkestrel/kestrel/issues/311).

## The boundary as built

- The operator listener defaults to `127.0.0.1:7718` and warns when bound beyond loopback
  ([`cli.rs`](../../crates/kestrel/src/cli.rs#L72-L81), [`serve.rs`](../../crates/kestrel/src/role/serve.rs#L69-L74)).
  The image binds `0.0.0.0:7718` inside the container, and compose publishes it only on the host's
  loopback ([Dockerfile](../../images/kestrel/Dockerfile#L33), [`compose.yaml`](../../compose.yaml#L50)).
- axum is compiled with `http1` and without `http2`
  ([`Cargo.toml`](../../crates/kestrel/Cargo.toml#L19)), and `axum::serve` accepts a plain TCP
  listener with no TLS ([axum `serve`](https://docs.rs/axum/0.8.9/axum/fn.serve.html)).
- The operator router checks no `Host` or `Origin` header and sends no CORS headers; every route sits
  under `/operator/` ([`operator.rs`](../../crates/kestrel/src/operator.rs#L124)).
- The Transcript stream is a `GET` whose cursor is read **only** from the `Last-Event-ID` request
  header; absent means from the beginning. Each entry is an `entry` event whose `id` is the cursor
  after it. A read ends with an `end` event that carries no `id`. A comment keeps the stream alive
  every 15 seconds. A bad cursor is refused with a JSON `Refusal` and a 4xx status
  ([`openapi/operator.json`](../../openapi/operator.json), [`operator.rs`](../../crates/kestrel/src/operator.rs#L1760-L1838)).
- The CLI already reads this stream as a fetch-style reader. It sends `Last-Event-ID` on its first
  request when it holds a cursor, reads the refusal body on a 4xx, stops on `end`, and retries every
  250 ms for up to 30 s ([`transcript.rs`](../../crates/kestrel-client/src/transcript.rs#L15-L16)).

## EventSource and a fetch reader

**EventSource takes no headers.** Its constructor accepts a URL and an `EventSourceInit` whose only
member is `withCredentials`
([HTML §9.2.2](https://html.spec.whatwg.org/multipage/server-sent-events.html#the-eventsource-interface)).

**`Last-Event-ID` is sent only when reconnecting.** The *last event ID string* starts empty. The
header is set only in *reestablish the connection*, and only when that string is non-empty
([HTML §9.2.3](https://html.spec.whatwg.org/multipage/server-sent-events.html#sse-processing-model),
[§9.2.4](https://html.spec.whatwg.org/multipage/server-sent-events.html#the-last-event-id-header)).
An `id:` field sets the string, and it keeps that value until the server sends another `id:`
([HTML §9.2.6](https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation)).
*Inference:* kestrel reads the cursor only from that header, so a new `EventSource` always replays
the Transcript from its first entry, even when the page already holds the entries up to some cursor.

**Reconnection.**

- Clean end of body: the user agent reestablishes the connection.
- Network error: it reestablishes, unless it knows retrying is futile.
- A status other than 200, or a `Content-Type` other than `text/event-stream`: it *fails the
  connection*. The state becomes `CLOSED` and it never retries
  ([HTML §9.2.2–9.2.3](https://html.spec.whatwg.org/multipage/server-sent-events.html#sse-processing-model)).
  The introduction names 204 as the way to stop a client from reconnecting
  ([HTML §9.2.1](https://html.spec.whatwg.org/multipage/server-sent-events.html#server-sent-events-intro)).
- The delay is a `retry:` value, or else an implementation-defined default. User agents may add
  exponential backoff.

The default is 3000 ms in Chromium
([`event_source.cc`](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/modules/eventsource/event_source.cc))
and WebKit ([`EventSource.cpp`](https://github.com/WebKit/WebKit/blob/main/Source/WebCore/page/EventSource.cpp)),
and 5000 ms in Firefox ([`EventSource.cpp`](https://github.com/mozilla-firefox/firefox/blob/main/dom/base/EventSource.cpp)).
An `error` event carries no status or body. The spec itself notes that "little to no information
can be made available in the events themselves"
([HTML §9.2.10](https://html.spec.whatwg.org/multipage/server-sent-events.html)). Named events such
as `entry` and `end` reach only listeners added for those names. `onmessage` fires only for the
default `message` type
([HTML §9.2.1](https://html.spec.whatwg.org/multipage/server-sent-events.html#server-sent-events-intro),
[§9.2.6](https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation)).
The interface is exposed in `Window` and in workers
([HTML §9.2.2](https://html.spec.whatwg.org/multipage/server-sent-events.html#the-eventsource-interface)).

*Inference for kestrel's stream:*

- After `end`, the server closes the body, so an `EventSource` reconnects with the last entry's id.
  For a sealed Session, kestrel answers that reconnect with `end` again. The cycle repeats every
  reconnection delay unless the page calls `close()` when it receives `end`.
- A refused cursor (4xx) or a 503 closes the `EventSource` for good, and the page cannot see why.

**What a fetch reader gains.**

- `Last-Event-ID` is not a forbidden request-header, so `fetch` may set it on the first request
  ([Fetch, forbidden request-header](https://fetch.spec.whatwg.org/#forbidden-request-header)).
- The reader sees the status and can read the JSON refusal.
- It stops through an `AbortSignal`.
- It chooses its own retry policy, as the CLI does.

**What a fetch reader costs.**

- It must parse `text/event-stream` itself: UTF-8 with an optional BOM; CR, LF and CRLF line ends;
  multi-line `data`; a trailing event without a blank line discarded
  ([HTML §9.2.5–9.2.6](https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream)).
- It must implement reconnection and the `retry:` field.

Cross-origin, `Last-Event-ID` is not a CORS-safelisted request-header, so a cross-origin `fetch`
that sets it needs a preflight
([Fetch, CORS-safelisted request-header](https://fetch.spec.whatwg.org/#cors-safelisted-request-header)).
Either way, a stream holds one HTTP connection for as long as it is open.

## Connection limits on a loopback listener

Each engine allows six HTTP/1.1 connections per host at once:

| Engine | Limit | Source |
| --- | --- | --- |
| Chromium | 6 per socket-pool group, which is host, port, protocol and privacy mode; WebSockets get 255 | [`client_socket_pool_manager.cc`](https://github.com/chromium/chromium/blob/main/net/socket/client_socket_pool_manager.cc), [life of a URLRequest](https://github.com/chromium/chromium/blob/main/net/docs/life-of-a-url-request.md#socket-pools) |
| Firefox | `network.http.max-persistent-connections-per-server` is 6 | [`all.js`](https://github.com/mozilla-firefox/firefox/blob/main/modules/libpref/init/all.js) |
| WebKit (CFNetwork path) | preferred connection count per host is 6 | [`ResourceRequestCFNet.cpp`](https://github.com/WebKit/WebKit/blob/main/Source/WebCore/platform/network/cf/ResourceRequestCFNet.cpp) |

In Chromium each Profile has its own network context
([life of a URLRequest](https://github.com/chromium/chromium/blob/main/net/docs/life-of-a-url-request.md)).
The pool decides which waiting requests get connected sockets first.

**The limit spans every tab.** MDN states it for SSE: without HTTP/2 the limit is "per browser +
domain", six across all tabs, and Chrome and Firefox closed the bug as "Won't fix". Over HTTP/2 the
number of streams is negotiated, 100 by default
([MDN EventSource](https://developer.mozilla.org/en-US/docs/Web/API/EventSource)). The HTML
standard warns about pages that each hold an `EventSource` to one domain. It lists three ways
around that: a unique domain name per connection, a per-page toggle, or sharing one `EventSource`
through a shared worker
([HTML §9.2.7](https://html.spec.whatwg.org/multipage/server-sent-events.html#authoring-notes)).
`SharedWorker` ships in every current engine: Chrome Android since 148, Safari since 16
([MDN SharedWorker](https://developer.mozilla.org/en-US/docs/Web/API/SharedWorker#browser_compatibility)).

**A browser cannot reach HTTP/2 on a plain-HTTP listener.** RFC 9113 says HTTP/2 support for an
`http` URI can be discovered only out of band, by "prior knowledge". It also deprecates the `h2c`
Upgrade token as "never widely deployed"
([RFC 9113 §3, §3.1, §3.3](https://www.rfc-editor.org/rfc/rfc9113#section-3)). The HTTP working
group's FAQ says "currently no browser supports HTTP/2 unencrypted"
([HTTP/2 FAQ](https://http2.github.io/faq/#does-http2-require-encryption)). A browser negotiates
`h2` only over TLS with ALPN
([RFC 9113 §3.2](https://www.rfc-editor.org/rfc/rfc9113#section-3.2)). kestrel's listener speaks
neither.

*Inference:*

- Every browser tab's open streams to `127.0.0.1:7718` share one pool of six.
- With *n* streams per tab, 6/*n* tabs fill it. After that, any further request to that host and
  port waits, including a POST and a page asset served from the same listener.
- `localhost:7718` and `127.0.0.1:7718` are different hosts, so they are separate groups, and also
  separate origins.

## TanStack Start

This section is checked against `@tanstack/react-start` 1.168.58 (npm, 23 September 2026). Docs and
source are cited at
[TanStack/router@bb4423e](https://github.com/TanStack/router/tree/bb4423e09872aee4f2544600d0eba3303fc7db56).

**It can emit a static SPA.** With `spa.enabled`, a build adds a prerender step:

- It prerenders the root route only, with the pending fallback where matched routes would render.
- It writes the result to `/_shell.html`.
- It relies on rewriting every 404 to that shell.

A host must serve existing static files first, may allow-list paths through to a server, and must
rewrite every other 404 to the shell
([SPA mode](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/guide/spa-mode.md)).
`prerender` also prerenders other routes to HTML. Automatic discovery skips routes with path
parameters ([Static prerendering](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/guide/static-prerendering.md)).
Prerendered pages, the shell included, are written into the client output directory
([`post-build.ts`](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/packages/start-plugin-core/src/post-build.ts#L28-L55),
[`prerender.ts`](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/packages/start-plugin-core/src/prerender.ts#L60)).
That directory is `dist/client` in the documented Vite build
([Hosting, Netlify](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/guide/hosting.md#netlify--official-partner)).
The shell is prerendered from the SSR build, and root-route loaders run while it is built
([SPA mode](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/guide/spa-mode.md#dynamic-data-in-your-shell)).
*Inference:* a JavaScript runtime is needed to build it, not to serve it.

**What goes without its server runtime.** These run in Start's server:

- full-document SSR;
- server functions, which reach the client as network requests to `/_serverFn/*`;
- server routes;
- request middleware;
- streaming from server functions.

([Overview](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/overview.md#should-i-use-tanstack-start-or-just-tanstack-router),
[Execution model](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/guide/execution-model.md),
[Server routes](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/guide/server-routes.md),
[Streaming from server functions](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/guide/streaming-data-from-server-functions.md).)
Route loaders are isomorphic and run in the browser during navigation. Start's own docs suggest
TanStack Router alone for an app that needs none of the server features
([Overview](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/overview.md#should-i-use-tanstack-start-or-just-tanstack-router)).

**What running its server adds.** The documented Node shapes are these:

- Vite with Nitro: `node .output/server/index.mjs`.
- Rsbuild: a fetch-style entry in `dist/server/index.js`, served with `srvx` alongside
  `dist/client`.
- Bun: a Nitro preset or a Bun server script.

([Hosting](https://github.com/TanStack/router/blob/bb4423e09872aee4f2544600d0eba3303fc7db56/docs/start/framework/react/guide/hosting.md#nodejs--docker).)
*Inference:* this is a second long-lived process beside the control plane. The control-plane image
today holds the `kestrel-control-plane` binary plus `ca-certificates`, `curl`, `docker-cli` and
`tzdata`, and no JavaScript runtime ([Dockerfile](../../images/kestrel/Dockerfile#L12-L24)).

*Inference, flagged for [Decide how the browser Client is built, served and kept same-origin](https://github.com/openkestrel/kestrel/issues/311):*
`CONTEXT.md` defines a Client as a program that reaches a control plane over a published boundary,
"holding no store of its own and executing nothing".
If a Start server called the operator boundary on the browser's behalf, that server would be the
program reaching the boundary. The browser would then reach Start's server functions, not the
boundary ADR-0015 publishes.

## TanStack Query

This section is checked against `@tanstack/query-core` 5.104.0. Source and docs are cited at
[TanStack/query@d4033eb](https://github.com/TanStack/query/tree/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca).

**`streamedQuery` is exported as `experimental_streamedQuery`**
([`index.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/index.ts#L47)).

- Its `streamFn` returns an `AsyncIterable`. Each chunk is folded into the cache with
  `setQueryData`, through a reducer; the default reducer appends to an array.
- The query is `pending` until the first chunk and `success` after it. It stays in fetch status
  `fetching` until the iterable ends
  ([reference](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/docs/framework/react/reference/functions/experimental_streamedQuery.md),
  [`streamedQuery.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/streamedQuery.ts#L68-L135)).
- `refetchMode` controls a refetch. `'reset'`, the default, erases the data and returns the query to
  `pending`. `'append'` adds to it. `'replace'` writes once the stream ends
  ([`streamedQuery.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/streamedQuery.ts#L14-L20)).
- A call counts as a refetch when the query `isFetched()`, meaning it has recorded any data or
  error update
  ([`streamedQuery.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/streamedQuery.ts#L86-L93),
  [`query.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/query.ts#L412-L414)).
- Each retry calls the query function again with a fresh context
  ([`query.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/query.ts#L650)).
- `streamedQuery` keeps no cursor. Its `streamFn` context carries `client` and `queryKey`
  ([`streamedQuery.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/streamedQuery.ts#L98-L110)).

*Inference:*

- A stream that fails partway is retried, and under `'reset'` it restarts from empty.
- Under `'append'` a retry appends whatever the new stream yields. It resumes without duplicates
  only if `streamFn` itself reads the cached entries to find the cursor to send.

**Refetch and retry defaults matter for a stream that does not end.**

- Failed queries are retried 3 times with exponential backoff.
- Stale queries refetch on mount, on window focus and on reconnect
  ([Important defaults](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/docs/framework/react/guides/important-defaults.md)).
- Focus and reconnect refetch with `cancelRefetch: false`, so they join a fetch already in flight
  ([`query.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/query.ts#L491-L505)).
- An explicit `refetch` or invalidation defaults to `cancelRefetch: true`. That cancels the running
  request and starts a new one
  ([`RefetchOptions`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/docs/framework/react/reference/interfaces/RefetchOptions.md)).

**Cancellation depends on consuming the signal.**

- By default, a query that unmounts before its promise resolves is not cancelled. It is cancelled
  only if the query function consumed its `AbortSignal`
  ([Query cancellation](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/docs/framework/react/guides/query-cancellation.md)).
- `streamedQuery` marks itself cancelled only when `streamFn` reads `signal`, and checks that flag
  only when the next chunk arrives
  ([`streamedQuery.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/streamedQuery.ts#L98-L120),
  [`utils.ts`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/packages/query-core/src/utils.ts#L594-L622)).

*Inference:* a follow stream whose `streamFn` ignores the signal keeps its connection open after the
component unmounts. That connection counts against the six above.

**Cache writes from an SSE handler.** `setQueryData` is the documented synchronous way to write
into the cache from outside a query function
([`QueryClient`](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/docs/framework/react/reference/classes/QueryClient.md#setquerydata),
[Updates from mutation responses](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/docs/framework/react/guides/updates-from-mutation-responses.md)).
One handler can therefore feed several query keys from a single stream. Query's only first-class
cursor is the `pageParam` of infinite queries, which is for fetching pages
([Infinite queries](https://github.com/TanStack/query/blob/d4033eb1e5bdef3c8aa72a6cc614bcdd98b1ddca/docs/framework/react/guides/infinite-queries.md)).

## Serving static assets from the control plane

kestrel locks axum 0.8.9 and tower-http 0.6.11. tower-http arrives only through reqwest's
`follow-redirect` feature, so its `fs` feature is off (`cargo tree -i tower-http -e features`).

**A directory read at run time.** tower-http's `ServeDir` does the following:

- guesses `Content-Type` from the file extension;
- answers 404 for missing files, `..`, backslashes and unreadable paths;
- can serve precompressed `gzip`, `br` or `zstd` siblings;
- returns 405 to methods other than `GET` and `HEAD` unless told otherwise;
- takes a `fallback`, or a `not_found_service` that forces 404, which its docs describe as the
  setup "often found in single page applications".

([`ServeDir`](https://docs.rs/tower-http/0.6.11/tower_http/services/struct.ServeDir.html).)
axum mounts such a service with `nest_service` or `fallback_service`, as its `static-file-server`
example does ([example](https://github.com/tokio-rs/axum/blob/main/examples/static-file-server/src/main.rs),
[`Router`](https://docs.rs/axum/0.8.9/axum/struct.Router.html#method.fallback_service)). A relative
directory resolves against the process's working directory. The image sets that to
`/var/lib/kestrel`, the data volume ([Dockerfile](../../images/kestrel/Dockerfile#L38)).

**Embedded at build.**

- `include_bytes!` reads a file at compile time, from a path relative to the current source file
  ([std docs](https://doc.rust-lang.org/std/macro.include_bytes.html)).
- `rust-embed` 8.12.0 embeds a folder in release builds, or in any build with its `debug-embed`
  feature. Without it, a debug build reads the files from disk on every call. A missing folder is a
  compile error unless `#[allow_missing = true]` is set
  ([README](https://docs.rs/crate/rust-embed/8.12.0/source/README.md)).
- Its README says the debug path resolves "relative to where the binary is run from". The source
  says otherwise. `rust-embed-impl` joins a relative `folder` onto the compile-time
  `CARGO_MANIFEST_DIR`, and the debug reader it generates holds that absolute path as a literal
  ([`rust-embed-impl` source](https://docs.rs/crate/rust-embed-impl/8.12.0/source/src/lib.rs)).

**What [Resolve crate root at run time, not compile time](https://github.com/openkestrel/kestrel/pull/290) implies.**
That PR replaced `env!("CARGO_MANIFEST_DIR")` in the tests with a path resolved at run time. The
old pattern compiled one checkout's path into a binary that another checkout then ran, while the two
shared a target directory. mise now pins `CARGO_TARGET_DIR` per checkout
([`mise.toml`](../../mise.toml#L10)). *Inference:*

- A directory the control plane serves at run time has to be named by run-time configuration, not
  by a compile-time path. The release image copies only the binary out of `/src`
  ([Dockerfile](../../images/kestrel/Dockerfile#L24)), so no compile-time path exists where it
  runs.
- `rust-embed`'s debug reader is the same compile-time pattern the PR removed.

**The build as it stands.** The image's build stage is the Rust image. It copies only
`rust-toolchain.toml`, `Cargo.toml`, `Cargo.lock` and `crates/`
([Dockerfile](../../images/kestrel/Dockerfile#L4-L9)). The repository declares a Bun workspace over
`packages/*` ([`package.json`](../../package.json), [`mise.toml`](../../mise.toml)), and no
`packages/` directory exists yet. *Inference:*

- Embedding needs the built assets present in that stage.
- Serving a directory needs them copied into the runtime image.

## The browser-to-loopback threat

**Simple requests are sent without asking.**

- The CORS-safelisted methods are `GET`, `HEAD` and `POST`. A `Content-Type` is safelisted only as
  `application/x-www-form-urlencoded`, `multipart/form-data` or `text/plain`
  ([Fetch, CORS-safelisted method](https://fetch.spec.whatwg.org/#cors-safelisted-method),
  [request-header](https://fetch.spec.whatwg.org/#cors-safelisted-request-header)).
- A request outside those rules gets a preflight
  ([Fetch, CORS-preflight fetch](https://fetch.spec.whatwg.org/#cors-preflight-fetch)).
- A request inside them is sent to any origin. CORS then governs only whether the page may read the
  response. OWASP names this as the CSRF risk, and warns specifically about JSON APIs that accept
  `text/plain`
  ([OWASP CSRF cheat sheet, disallowing simple requests](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html#disallowing-simple-requests)).

**The boundary today.**

- axum's `Json` extractor rejects a request without an `application/json`-type `Content-Type`
  ([`Json`](https://docs.rs/axum/0.8.9/axum/struct.Json.html)). kestrel maps that rejection to 400
  ([`operator.rs`](../../crates/kestrel/src/operator.rs#L1877)). So no JSON-body route runs on a
  form or `text/plain` forgery.
- Four state-changing routes take no body: seal a Session, stop a Run, enable a Trigger, disable a
  Trigger
  ([`operator.rs`](../../crates/kestrel/src/operator.rs#L1268-L1287),
  [seal](../../crates/kestrel/src/operator.rs#L1614),
  [stop](../../crates/kestrel/src/operator.rs#L1670)).
  A bodiless cross-origin `POST` is a simple request, so it reaches them.
- `PUT` and `DELETE` need a preflight. axum answers a method with no handler, `OPTIONS` included,
  with 405 ([`MethodRouter::new`](https://docs.rs/axum/0.8.9/axum/routing/method_routing/struct.MethodRouter.html#method.new)).
  The preflight therefore fails, and the request is never sent.
- A cross-origin page cannot read any response, because the boundary sends no CORS headers.

**Origin and Host are set by the browser, not by page script.** Both are forbidden
request-headers ([Fetch](https://fetch.spec.whatwg.org/#forbidden-request-header)). `Origin` is
sent on every CORS-mode request and on every non-`GET`/`HEAD` request. A same-origin `GET` or
`HEAD` carries none
([Fetch, append a request `Origin` header](https://fetch.spec.whatwg.org/#append-a-request-origin-header)).
The CLI's requests set no `Origin`
([`transcript.rs`](../../crates/kestrel-client/src/transcript.rs#L76-L81)).

**DNS rebinding.** An attacker's page on `http://evil.example:7718` re-resolves its own name to
`127.0.0.1`, which makes the page same-origin with the loopback listener. Its requests then carry
`Host: evil.example:7718`. The DNS-rebinding paper names rejecting unexpected `Host` headers as a
server-side defence
([Jackson et al., *Protecting Browsers from DNS Rebinding Attacks*](https://crypto.stanford.edu/dns/dns-rebinding.pdf)).
Vite's CVE-2025-24010 is this attack against a plain-HTTP localhost dev server. Its fix is an
allowlist of Hosts: `localhost`, `*.localhost` and IP addresses by default, skipped over HTTPS
([GHSA-vg6x-rcgg-rjx6](https://github.com/advisories/GHSA-vg6x-rcgg-rjx6),
[Vite `server.allowedHosts`](https://vite.dev/config/server-options.html#server-allowedhosts)).

**Which check defeats which.** This is derived from the facts above.

| Attack | `Host` allowlist | `Origin` allowlist | `Origin` equals `Host` |
| --- | --- | --- | --- |
| Cross-origin simple request (page on another site → `127.0.0.1:7718`) | Does not stop it: `Host` is legitimate | Stops it: a `POST` always carries the attacker's `Origin` | Stops it |
| DNS rebinding, state-changing `POST` | Stops it | Stops it | Does not: both name the attacker |
| DNS rebinding, same-origin `GET` (Transcript, lists) | Stops it | Does not: no `Origin` is sent | Does not |
| A page on `http://localhost:<other port>` | Does not | Stops it only on an exact origin match | Stops it |

The last row holds because the HTML standard ignores ports for *same site*
([HTML, same site](https://html.spec.whatwg.org/multipage/browsers.html#same-site)). So another local
port arrives as `Sec-Fetch-Site: same-site`. A Fetch Metadata policy that rejects only `cross-site`,
the default OWASP describes, lets it through
([OWASP, Fetch Metadata](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html#fetch-metadata-headers)).
Non-browser clients send no `Origin`. An `Origin` check that rejects a missing header would
therefore refuse the CLI and the compose health check.

**Chromium's Local Network Access.**

- Since Chrome 142, a request from a public page to a local-network or loopback address needs the
  user's permission. That covers `fetch`, subresources and subframes
  ([Chrome for Developers](https://developer.chrome.com/blog/local-network-access)).
- The check applies to the IP address actually connected to, for every connection, so it covers
  rebinding
  ([Local Network Access, draft CG report](https://wicg.github.io/local-network-access/)).
- The spec says Chromium implements it only for requests from public pages, and names no other
  engine. Loopback-to-loopback is not gated.
- It is a user prompt, not a control the server holds.

## Left undecided

This note settles none of the following.

- **How changes reach the Client.** The choices include `EventSource`, a fetch reader, a
  `SharedWorker` shared across tabs, and whether the boundary accepts a cursor other than the
  header. This belongs to
  [Decide how the Client learns that Sessions, Runs and the queue changed](https://github.com/openkestrel/kestrel/issues/310).
- **How the Client is built and served.** The choices include a static Start SPA the control plane
  serves, a Start server, TanStack Router alone, embedding or a run-time directory, which `Host` and
  `Origin` checks to adopt, and what becomes of the four bodiless POST routes. This belongs to
  [Decide how the browser Client is built, served and kept same-origin](https://github.com/openkestrel/kestrel/issues/311).
