# Acceptance: kestrel opens its own PRs

The procedure for `0.1`'s acceptance test ([#47](https://github.com/jtmthf/kestrel/issues/47)): real
Docker, real opencode, a real `ready-for-agent` issue on this repository worked with no human action
after the labelling, a real pull request landing, and the `kill -9` test run against that same live
stack rather than only against the harness.

## 1. Build the GitHub-capable environment image

`kestrel-env` carries only the supervisor, opencode and git
([images/kestrel-env/README.md](../images/kestrel-env/README.md)). An agent that has to open a pull
request on the repository it just worked needs a way to call GitHub, which nothing in that image
provides. `images/kestrel-env-github/Dockerfile` is the derived image that adds `gh`, the same
pattern the README already documents for `claude-code-acp`:

```sh
docker compose build
docker build --file images/kestrel-env-github/Dockerfile --tag kestrel-env-github .
```

## 2. Point the stack at it

`compose.yaml` defaults `KESTREL_IMAGE` to `kestrel-env`. Override it for the `kestrel` service with
a local, gitignored `compose.override.yaml` — Compose merges it automatically and it is not part of
the shipped stack:

```yaml
name: kestrel
services:
  kestrel:
    environment:
      KESTREL_IMAGE: kestrel-env-github
```

```sh
docker compose up -d
```

## 3. Declare the organization, workspace and agent

```sh
alias kestrel='docker compose exec kestrel kestrel'
kestrel organization declare acme
kestrel workspace declare kestrel --organization acme \
  --repository https://github.com/jtmthf/kestrel --branch main
kestrel agent declare builder --organization acme
```

## 4. Give the environment a way to reach GitHub

```sh
kestrel credential set GH_TOKEN --organization acme <<< "$(gh auth token)"
```

`gh` inside the environment reads `GH_TOKEN` from its process environment on its own — nothing sets
it up further. The credential crosses the link at the spawn exactly like a model provider key does
([ADR-0010](adr/0010-a-provider-credential-crosses-the-link-at-the-spawn.md)); `GH_TOKEN` is not a
name kestrel treats specially, it is simply the variable `gh` already looks for. The token needs
`repo` scope to open a pull request — and, per the findings below, that scope is also enough to merge
one.

## 5. Register the integration and the trigger

```sh
kestrel integration register github origin --organization acme \
  --repository jtmthf/kestrel --token "$(gh auth token)"
kestrel trigger declare ready --organization acme --repository jtmthf/kestrel \
  --label ready-for-agent --workspace kestrel --agent builder
```

[Finding 2](#what-this-run-found) is what this cost on the day: the first poll records a page of
what has already happened on the repository, and declaring the trigger then swept all of it. A
trigger now fires only for events recorded after it was declared
([#106](https://github.com/jtmthf/kestrel/issues/106)), so registering the integration before the
trigger — the order above — leaves that history alone.

## 6. Label an issue, and watch

```sh
kestrel event list --organization acme
kestrel session list --organization acme
kestrel run list --session <session>
docker logs -f kestrel-<run>
```

Within a poll interval a session opens, a run is enqueued and dispatched into a real container, and
— once the agent finishes — a pull request lands and the originating issue gets the outcome comment,
all without anyone acting on the label a second time.

## 7. The `kill -9` test

With a run active:

```sh
docker kill -s SIGKILL kestrel-kestrel-1
docker start kestrel-kestrel-1   # or wait for the compose restart policy
kestrel session transcript <session>
```

Restarted inside the 2-minute lease (`LEASE` in `crates/kestrel/src/work.rs`), the run reconnects on
its own and finishes, and the transcript is unbroken across the restart — no missing or duplicated
entries. Restarted later than that, the lease expires first and the run ends `failed: the environment
stopped holding the run's lease out, and it expired`, which is the lease doing what it is for rather
than a defect: an environment that outlives its control plane's absence is not left holding the
session's one active-run slot forever.

## What this run found

1. **`images/kestrel-env-github` had to be built; nothing in the shipped path could reach GitHub's
   API before this.** The base image's own README already named its contents exhaustively — this
   confirmed the gap was real rather than assumed, and closed it as a derived image rather than by
   changing what the base image carries.
2. **A trigger declared against a repository's existing label history replays that history.**
   Declaring a `ready-for-agent` trigger against this repository's real backlog on a fresh database
   fired all thirteen already-`ready-for-agent` issues at once, because trigger matching runs over
   every unmatched occurrence, not only new ones. One had already ended (failed, by a deliberate
   `kill -9` of the run's own environment) and posted an outcome comment before this was caught and
   the trigger disabled. This run's actual demonstration used a dedicated `kestrel-acceptance-demo`
   label instead, applied to one issue only, specifically to avoid re-running this against the real
   backlog. A maintainer turning this on for real, for the actual backlog, will hit the same sweep on
   day one — which is arguably correct once that is a deliberate decision, but it is not something
   `kestrel trigger declare` warns about, and it is worth knowing before the first `declare` on a
   repository that already labels issues.
3. **A `repo`-scoped `GH_TOKEN` grants merge, not only opening a pull request.** During this
   procedure's second `kill -9` rehearsal, the agent woke into a session whose transcript already
   showed its own earlier, still-open pull request, decided the remaining work was to land it, and
   merged it — nothing kestrel asked it to do. Kestrel supplies the credential and stays out of what
   the run does with it (`ROADMAP.md`: "what happens inside a run is the run's business, not the
   session's"), so an operator who wants a human merge gate needs a token scoped to opening pull
   requests without merge rights, or branch protection requiring review, rather than relying on the
   agent to leave merging to someone else.

## What ran, concretely

- Issue [#94](https://github.com/jtmthf/kestrel/issues/94), labelled both `ready-for-agent` and the
  dedicated `kestrel-acceptance-demo` (the trigger matched on the latter, for the reason in finding 2).
- Pull request [#95](https://github.com/jtmthf/kestrel/pull/95), opened by the agent with no human
  action after the label and later merged by it, closing #94.
- `kill -9` against `kestrel-kestrel-1` mid-run, twice: restarted after the lease window (the run
  correctly ended failed) and restarted at 13 seconds (the run finished, transcript gap-free).

## The expensive-to-retrofit list

`ROADMAP.md`'s `0.1` section names five things as invisible on the day they ship and costly to add
later: the `Organization` column on every durable record, the transcript's entry granularity and its
current-values-not-replayed state, the `sealed` state on a session, the run-held lease, and the
bounded-window-plus-paging transcript read. All five are in place and exercised above (the lease by
the `kill -9` test directly, the paged transcript read by `kestrel session transcript`, sealing by
`0.1`'s own USAGE.md walkthrough). Nothing on that list was skipped.
