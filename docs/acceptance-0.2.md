# Acceptance attempt: kestrel works the backlog

The live run for `0.2`'s acceptance ticket
([#111](https://github.com/jtmthf/kestrel/issues/111)) proved parallel dispatch, its limit,
non-retroactive Trigger declaration, and idle sealing. It did not close the rung: two successful
Runs lost all of their work when their disposable Environments were reaped. That finding became
[#124](https://github.com/jtmthf/kestrel/issues/124) and then the durable-Instance specification
[#125](https://github.com/jtmthf/kestrel/issues/125). The `ROADMAP.md` marker remains at `0.2` until
that work lands and this acceptance run is repeated.

The test used the real Compose stack, GitHub integration, and `ready-for-agent` issues on this
repository. It began at commit `941ee0a`, the last planned implementation ticket in the rung, and
preserved the named volume from earlier dogfood runs so Trigger declaration was tested against live
history rather than an empty database.

## 1. Build and upgrade the live stack

The current control plane and supervisor were built with the GitHub-capable Environment image:

```sh
docker compose build
docker build --file images/kestrel-env-github/Dockerfile --tag kestrel-env-github .
docker compose up -d
```

A local, gitignored `compose.override.yaml` selected `kestrel-env-github`. Recreating the
control-plane container left the `kestrel` volume intact. The existing `acme` organization,
`kestrel` workspace, `builder` agent, `origin` GitHub integration, two sealed Sessions, and recorded
Events all survived. The default concurrency limit of two was unchanged.

The base image does not put Rust on its default `PATH`. OpenCode provisioned Rust 1.96.0 under
`/home/kestrel/.cargo/bin`, and the agents ran Cargo through its generated environment script.
An initial base-shell inspection missed that distinction; the resulting report
[#122](https://github.com/jtmthf/kestrel/issues/122) was corrected and closed.

## 2. Declare the Trigger against recorded history

Before declaration the store contained a `ready-for-agent` Event for
[#109](https://github.com/jtmthf/kestrel/issues/109), recorded on September 9, and only the two
sealed Sessions. The Trigger was declared on September 10:

```sh
kestrel trigger declare ready --organization acme --repository jtmthf/kestrel \
  --label ready-for-agent --workspace kestrel --agent builder
```

Six observations over a full GitHub poll interval continued to show the same two Sessions. Trigger
`ready` (`01a08d5d-24fc-7ad3-9030-e65f629bdda4`) opened nothing for #109 or any other Event recorded
before its declaration.

## 3. Put three issues on the backlog together

Issues [#79](https://github.com/jtmthf/kestrel/issues/79),
[#80](https://github.com/jtmthf/kestrel/issues/80), and
[#104](https://github.com/jtmthf/kestrel/issues/104) were unblocked and agent-ready. Their labels
were removed, then `ready-for-agent` was added to all three in parallel. Nobody opened a Session,
enqueued a Run, or directed an agent after that labelling.

One poll recorded all three Events and opened all three Sessions at `2026-09-10T22:10:12Z`.

| Issue | Session | Run | First observed state |
| --- | --- | --- | --- |
| #80 | `01a08d5f-10fa-7410-aeb3-c0e22a67379b` | `01a08d5f-10fb-7823-ba23-ec1632dc4ea9` | active |
| #104 | `01a08d5f-10fd-7200-b753-f78df4cb6d99` | `01a08d5f-10fd-7200-b753-f79f0c8f61ef` | active |
| #79 | `01a08d5f-10fe-76f1-bddc-ba282e7a271b` | `01a08d5f-10ff-7962-9395-1d349e6ecb28` | queued |

Docker showed exactly the two active Runs' containers. A third never appeared while both slots were
held. When #104 ended at `22:13:48Z`, #79 became active on the next sweep while #80 remained active.
The live run therefore exercised both sides of the limit: excess work waited, then dispatched as
soon as capacity became available.

## 4. Outcomes

All three initial Runs reported `succeeded`. #104 opened
[PR #121](https://github.com/jtmthf/kestrel/pull/121) and its outcome returned to the issue without
operator action. Its agent also wrote an ordinary issue comment before the Run ended; the integration
treated that as an operator follow-up and ran the Session a second time. That feedback hazard is the
non-blocking finding [#123](https://github.com/jtmthf/kestrel/issues/123).

The #79 and #80 Runs made and tested substantial repository changes, but neither committed or pushed
them. #80's final report explicitly said no commit or pull request was made because none was
requested. Kestrel recorded success and destroyed both Environments, leaving no recoverable copy of
either change. A process exit was successful, but the backlog issues were not durably worked.

This is the blocking finding [#124](https://github.com/jtmthf/kestrel/issues/124). Its accepted design
is [#125](https://github.com/jtmthf/kestrel/issues/125): a Session-owned Instance outlives its Runs,
the supervisor reports git state, and Kestrel never reaps an Instance holding work that exists
nowhere else. A prompt asking an agent to commit would improve one agent's odds, but it would not
provide the observable durability guarantee #125 requires.

## 5. Idle sealing

The named volume and stack were left running. All three Sessions sealed themselves after their real
24-hour idle windows, without an explicit `session seal`:

| Issue | Last active | Sealed |
| --- | --- | --- |
| #80 | `2026-09-10T23:04:53Z` | `2026-09-11T23:09:55Z` |
| #104 | `2026-09-10T22:48:10Z` | `2026-09-11T22:53:35Z` |
| #79 | `2026-09-10T22:48:28Z` | `2026-09-11T22:53:35Z` |

A later `ready-for-agent` Event opened a Session for #125. Its Run held a slot from
`2026-09-10T22:51:01Z` until its lease expired at `2026-09-11T01:12:00Z`, then that Session also
sealed itself at `2026-09-12T01:12:00Z`. The failed Run is further live evidence for #125's rule
that missing supervisor state must be treated as unrecoverable rather than safe to destroy.

## Result

The scheduler behavior requested by #111 worked live: one labelling batch opened three Sessions,
two Runs overlapped under a limit of two, the queued Run took the released slot, declaration did not
replay retained history, and the Sessions later sealed themselves. The rung remains open because
successful Runs can still destroy their only copy of completed work. After #125 lands, repeat the
durability portion of this run and move the `ROADMAP.md` marker to `0.3` only if several backlog
issues leave recoverable branches or pull requests.
