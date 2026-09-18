# Using kestrel

This walks from an empty machine to a run — kestrel provisioning an isolated container, cloning a
repository into it, and driving a coding agent there. Everything below works today. What does not
work yet is telling that agent what to do, which is the whole of
[Where this stops](#where-this-stops).

kestrel is at rung `0.1`. [`ROADMAP.md`](ROADMAP.md) is the order the rest arrives in.

## Before you start

- **Docker**, with Compose v2. Nothing else — kestrel asks for no configuration of its own, and
  there is nothing for you to supply.
- **An amd64 or arm64 machine.** The image a run executes in is built for those two, and the build
  fails on anything else rather than producing something that will not start.
- **A clone of this repository.** The stack builds from source rather than pulling images.

The first `docker compose up` builds three images and takes a few minutes. Every start after it
takes seconds.

## Bring it up

```sh
git clone https://github.com/jtmthf/kestrel
cd kestrel
docker compose up -d
```

Three images are built and three containers start: the **control plane**, which holds the database
and dispatches work; the filtered proxy it reaches the Docker daemon through; and the image a run
executes in, whose container exits at once because nothing runs it until a run needs one. The
database is on a named volume, so it outlives the containers.

Drop the `-d` to watch the logs in the foreground, or run `docker compose logs -f kestrel`.

Every command below reaches the control plane's CLI, which lives inside that container:

```sh
alias kestrel='docker compose exec kestrel kestrel'
```

The alias is for readability only. The real command is
`docker compose exec kestrel kestrel <something>`, and it is worth remembering that it reaches into
a container, because that is where to look when something goes wrong.

## Declare what the work happens against

Three declarations, in this order, because each needs the one before it. Each prints the identifier
of what it declared. A declaration describes what should exist, so running one again is safe: an
unchanged one changes nothing, and a changed one updates the workspace or agent by that name in
place, keeping its identifier.

An **organization** is the outermost boundary. Every record kestrel keeps belongs to exactly one:

```sh
kestrel organization declare acme
```

A **workspace** is what a session's work happens against — repositories and the base branch each
session's own branch is cut from. Repeat `--repository` to name more than one.

```sh
kestrel workspace declare kestrel \
  --organization acme \
  --repository https://github.com/jtmthf/kestrel \
  --branch main
```

An **agent** is a configured identity rather than a running process: the agent runtime that drives
it, and the model it works with.

```sh
kestrel agent declare builder --organization acme
```

Naming no `--model` asks for whatever the agent runtime's own default is, which is what you want
here; the run records which model that turned out to be. Name a specific model and the run fails at
model selection unless that runtime offers it.

Changing an agent's model is configuration rather than a rebuild, and a run already in flight stays
on the model it was dispatched with:

```sh
kestrel agent model builder --organization acme --model anthropic/claude-opus-4-5
```

Once a run has worked, kestrel knows what that runtime advertised, and refuses a model outside it
where you declare it rather than where it would be dispatched.

`--runtime` names the agent runtime: `opencode` unless you say otherwise, or `claude` or `codex`.
The work role maps each name to the command an environment spawns and speaks ACP to, which by
default is `opencode acp`, `claude-agent-acp` and `codex-acp`; the `kestrel-dev` image carries all
three. Set `KESTREL_AGENT_RUNTIME` on the control plane, or pass `--agent-runtime NAME=COMMAND`
repeatedly, to change the table. A run whose agent names a runtime missing from it fails and says
which.

```sh
kestrel agent declare codex --organization acme --runtime codex
```

A session takes its agent's runtime and model when it opens and keeps them while it is open:
redeclaring the agent, or changing its model, changes the sessions opened after that.

`kestrel organization list`, `kestrel workspace list --organization acme` and
`kestrel agent list --organization acme` show what you have declared.

## Open a session

A **session** is the durable thread of work. It survives restarts, owns a transcript, and contains
many runs over its life.

```sh
kestrel session open --organization acme --workspace kestrel --agent builder
```

Keep the identifier it prints — everything below takes it.

The session fixes the workspace's repositories as they are now and declares a branch of its own,
`kestrel/<session>`, so sessions opened side by side never work on one another's branch. Pass
`--branch` to work on an existing branch instead. Before the agent starts, the supervisor on the
session's instance clones each repository and checks that branch out, cutting it from the
workspace's when the repository does not have it yet; a checkout that fails ends the run naming the
repository and the branch. A later run on the same instance finds the checkout exactly as the run
before it left it, pushed or not. The control plane itself runs no git.

```sh
kestrel session show 01a07846-49fa-7dc0-a44b-183a63794ee3
```

```
session       01a07846-49fa-7dc0-a44b-183a63794ee3
organization  acme
workspace     kestrel
agent         builder
runtime       opencode
model         -
branch        kestrel/01a07846-49fa-7dc0-a44b-183a63794ee3
base          main
state         open
opened        2026-09-06T19:51:07.514310886Z
last active   2026-09-06T19:51:07.514310886Z
```

It already has a transcript, because opening it put the agent in as a participant:

```sh
kestrel session transcript 01a07846-49fa-7dc0-a44b-183a63794ee3
```

```
1  2026-09-06T19:51:07.514407094Z  participant joined  builder
```

## Enqueue a run

A **run** is one execution of an agent runtime on its session's instance. At most one is ever active
in a session.

```sh
kestrel run enqueue --session 01a07846-49fa-7dc0-a44b-183a63794ee3
```

Within seconds the control plane claims it, provisions a container for the session, and starts a
supervisor in it that clones the workspace's repositories and spawns an agent runtime, dialling back
over the link. The container is the session's **instance**: every later run in the session starts a
supervisor of its own in the same one.

The work role dispatches up to two runs at once by default. That conservative default leaves room on
a laptop for two repository checkouts, supervisors, and agent runtimes. Set
`KESTREL_MAX_ACTIVE_RUNS` on the control-plane container, or pass `--max-active-runs RUNS`, to choose
a different positive limit; runs beyond it remain queued until active ones end.

```sh
kestrel run list --session 01a07846-49fa-7dc0-a44b-183a63794ee3
```

```
01a07846-5d97-7230-9315-bfef2a644006  docker/kestrel-01a07846-5d97-7230-9315-bfef2a644006  -  active
```

The second column is the instance, and the third is the model the run is on, which it says once
the turn is over. The instance is a real container, and the run's supervisor in it says what it is
doing in the control plane's log:

```sh
docker compose logs -f kestrel
```

```
INFO kestrel::role::work: supervisor started run=01a07846-5d97-7230-9315-bfef2a644006
INFO kestrel::role::work: link open run=01a07846-5d97-7230-9315-bfef2a644006
INFO kestrel::role::work: reported connected run=01a07846-5d97-7230-9315-bfef2a644006
INFO kestrel::role::work: instruction start 1 run=01a07846-5d97-7230-9315-bfef2a644006
INFO kestrel::role::work: reported started 1 run=01a07846-5d97-7230-9315-bfef2a644006
```

The agent is now working — reading the repository, running commands, taking turns. It has no task,
though: nothing yet carries one to a run, so every run asks its agent the same fixed question and it
does whatever it infers from the repository it woke up in. There is no reason to wait for it to
decide it has finished. Carry on to the next section, which ends it.

**Where the model call goes.** You supplied no provider credentials and the run reached a model
anyway: opencode falls back to its own hosted provider when it has none of its own, so the contents
of the cloned workspace are read by inference that is not running on your machine.

**The control plane never holds the Docker socket.** It provisions that container through a proxy
that forwards the requests the compute driver makes and refuses everything else, and nothing an
agent runs can reach the proxy at all. That is the shipped default rather than a hardening step;
[ADR-0009](docs/adr/0009-the-daemon-is-reached-through-a-filtered-proxy.md) lists what is allowed and
says plainly what the filter does not buy.

## Your sessions survive a restart

A session is durable from the moment it is opened. A run in flight is not. Bring the whole stack
down and back up to see both:

```sh
docker compose down
docker compose up -d
kestrel session transcript 01a07846-49fa-7dc0-a44b-183a63794ee3
```

```
1  2026-09-06T19:51:07.514407094Z  participant joined  builder
2  2026-09-06T19:51:13.316822628Z  run started  01a07846-5d97-7230-9315-bfef2a644006
3  2026-09-06T19:58:58.48925017Z  run ended  01a07846-5d97-7230-9315-bfef2a644006  failed: the control plane stopped while this run was in flight
```

The session and its transcript are intact. The run that was executing ended with an explicit status
rather than staying active forever, and its supervisor was stopped. The session's instance is still
there, with the checkout as the run left it, for the session's next run.

Nothing reaps an instance yet: it outlives its session's sealing too, until you remove its container
yourself. If an instance is gone when a run needs it, that run fails and says that whatever the
instance held that was never pushed is lost; the next run provisions a fresh instance and checks the
session's branch out from the remote.

`docker compose down --volumes` removes the named volume too, and with it every session, transcript
and declaration on this machine. It is the only command here that destroys anything.

## Reading a long transcript

A transcript is read in bounded windows, so a session with thousands of entries costs the same to
read as one with three.

```sh
kestrel session transcript 01a07846-49fa-7dc0-a44b-183a63794ee3 --window 2
```

```
1  2026-09-06T19:51:07.514407094Z  participant joined  builder
2  2026-09-06T19:51:13.316822628Z  run started  01a07846-5d97-7230-9315-bfef2a644006
```

The cursor the next window resumes from is written to **stderr**, so it never runs together with the
entries on stdout:

```
cursor  01a07846-49fa-7dc0-a44b-183a63794ee3:2
```

Pass it back to continue:

```sh
kestrel session transcript 01a07846-49fa-7dc0-a44b-183a63794ee3 \
  --cursor 01a07846-49fa-7dc0-a44b-183a63794ee3:2
```

```
3  2026-09-06T19:58:58.48925017Z  run ended  01a07846-5d97-7230-9315-bfef2a644006  failed: the control plane stopped while this run was in flight
```

## Sealing a session

Sealing ends a session without deleting it. A sealed session stays readable and is never reopened.

```sh
kestrel session seal 01a07846-49fa-7dc0-a44b-183a63794ee3
```

It accepts no further runs:

```
Error: the session 01a07846-49fa-7dc0-a44b-183a63794ee3 is sealed, and accepts no run
```

A session seals itself too. `last active` moves when the session opens, when a run is enqueued into
it, and when one of its runs ends; a session that has sat at the same `last active` for 24 hours with
no run holding its slot is sealed by kestrel, exactly as the command above would have.

Work that would have continued it starts a new session that records the sealed one:

```sh
kestrel session open --organization acme --workspace kestrel --agent builder \
  --continues 01a07846-49fa-7dc0-a44b-183a63794ee3
```

Both ends of that link are visible. The new session shows what it continues, and the sealed one
gains a `continued-by` line:

```
session       01a0784e-7844-7ee3-a273-9f032047aeb4
organization  acme
workspace     kestrel
agent         builder
state         open
opened        2026-09-06T20:00:03.652235801Z
last active   2026-09-06T20:00:03.652235801Z
continues     01a07846-49fa-7dc0-a44b-183a63794ee3
```

## Let a label start the work

Every session above you opened by hand. A **trigger** is the standing rule that opens one for you:
what it matches, and the agent and workspace it starts that work with.

kestrel has to be able to see the repository first. An **integration** is a credentialed connection
to an external system, and it declares which directions it carries — events inbound, kestrel's
requests outbound, or both:

```sh
kestrel integration register github origin \
  --organization acme \
  --repository jtmthf/kestrel \
  --token ghp_your_token
```

It discovers events by polling every minute, so nothing here needs an inbound address or a tunnel. A
first poll reads one page and stops, so an integration starts from roughly the moment you register it
rather than walking the repository's whole back history.

If GitHub can reach kestrel, give the integration the secret you configure on the repository's
webhook instead, with `--webhook-secret`. kestrel then stops polling and receives the repository's
events as GitHub delivers them. `kestrel integration list` shows the path to point the webhook at,
`/webhooks/<integration id>`, with content type `application/json`. A delivery whose
`X-Hub-Signature-256` does not verify is refused and never recorded.

### Events from anything else

Anything that can POST can start work, through a generic webhook:

```sh
kestrel integration register webhook ci --organization acme --secret "$KESTREL_WEBHOOK_SECRET"
```

A sender presents the secret as `Authorization: Bearer <secret>` and POSTs to the integration's
path. A CloudEvent in either HTTP mode keeps the `id`, `source` and `type` its sender gave it: binary
mode (`ce-*` headers, the body is `data`) or structured mode (`application/cloudevents+json`, the
body is the whole event). Any other POST is wrapped: its type is `dev.kestrel.webhook.received`, its
source is the path it was posted to, and its body is `data`, as JSON when it is JSON and as text
otherwise.

```sh
curl -X POST "http://127.0.0.1:7717/webhooks/$INTEGRATION" \
  -H "Authorization: Bearer $KESTREL_WEBHOOK_SECRET" \
  -H 'ce-specversion: 1.0' -H 'ce-id: build-7' \
  -H 'ce-source: https://ci.example.com/pipelines/3' -H 'ce-type: com.example.build.failed' \
  -H 'Content-Type: application/json' -d '{"step": "test"}'
```

kestrel answers `202 Accepted` once the event is recorded, and matches it afterwards. An event
delivered twice with the same `source` and `id` is recorded once.

```sh
kestrel event list --organization acme
```

```
01a07c31-4d0c-7b91-88f1-2f1a9c0b3e77  2026-09-07T14:01:58Z  jtmthf/kestrel  labeled  ready-for-agent  #44  0.1/21: The GitHub Trigger opens a Session from an Event
```

Now the rule itself. A trigger decides what an agent does to a repository with your
organization's credentials, so declare it in a file you keep in version control and review in a
pull request, `.kestrel/triggers.yaml`:

```yaml
triggers:
  ready:
    filter:
      all:
        - exact: {source: "https://github.com/jtmthf/kestrel"}
        - exact: {type: com.github.issues.labeled}
        - exact: {data.label.name: ready-for-agent}
        - exact: {data.issue.author_association: MEMBER}
    brief: |
      Work {{ event.data.issue.html_url }}: {{ event.data.issue.title }}
    branch: kestrel/issue-{{ event.data.issue.number }}
    workspace: kestrel
    agent: builder
```

kestrel cannot read that file out of the repository for itself: reading it takes a checkout, a
checkout takes a session, and a session takes a trigger. So you apply it, and kestrel prints the
diff it makes:

```sh
kestrel trigger apply --organization acme -f .kestrel/triggers.yaml
```

```
+ ready
    matches
      + source = "https://github.com/jtmthf/kestrel" and type = "com.github.issues.labeled" and data.label.name = "ready-for-agent" and data.issue.author_association = "MEMBER"
    workspace
      + kestrel
    agent
      + builder
    branch
      + kestrel/issue-{{ event.data.issue.number }}
    brief
      + Work {{ event.data.issue.html_url }}: {{ event.data.issue.title }}
```

The file is the whole of what applies: a trigger you change in it is changed, a trigger you take
out of it is removed, and applying the same file again prints `no changes`. Add `--dry-run` to see
the diff without making it, and `-f -` to read the file from standard input. An apply is one
transaction, so a file naming an agent or workspace that does not exist changes nothing. Keep one
declaration file per organization: an apply removes every trigger an earlier apply made that this
file does not declare.

What a trigger matches is a filter over the event's CloudEvents attributes — `exact`, `prefix` and
`suffix` over `id`, `source`, `specversion`, `type`, `subject` and `time`, combined with `all`,
`any` and `not` — and kestrel extends it with paths into the event's `data`. A trigger names the
repository by its `source`, never the integration that saw the event, so it keeps matching whether
kestrel learned of the event by polling or by webhook. Comparisons are case-sensitive, and a path
into `data` that leads nowhere matches nothing.

The `brief`, the `branch` and the optional `correlation` are
[minijinja](https://docs.rs/minijinja) templates over `event`, rendered from the event and never
choosing anything the declaration names. Leave `branch` out and the session declares one of its own.
Rendering is strict: a field the event does not have is an error, not an empty string, so a brief
that says `on {{ event.data.pull_request.head.ref }}` over a labelled issue fails rather than
rendering `on `. Ask first with `{% if event.data.pull_request is defined %}`. A template runs
inside the control plane, so how much work it does, how deep it recurses and how much it writes
are all bounded.

A `correlation` requires `on_miss: open` or `on_miss: ignore`. A hit feeds the open Session that
holds the key; its configured Agent stays fixed. A key only a sealed Session held is still
kestrel's work, so either setting opens a new Session continuing the most recently sealed one. For a
key no Session has held, `open` starts a new Session and `ignore` records the firing but starts no
work. A comment that follows up a sealed Session feeds the open one holding its key, or opens a
continuation that holds it.

### Letting a label choose the agent

A trigger starts its work with its `agent`, unless a label on the issue names another it `allows`:

```yaml
    agent: builder
    allows: [codex, claude]
```

An issue labelled `agent:codex` when the trigger fires opens its session with `codex`. The label
only chooses among agents the declaration names, so an issue cannot reach an agent you did not
review. Two `agent:` labels naming different agents, or one naming an agent the trigger does not
allow, open nothing: the firing fails, and `kestrel event show` prints why under `firings`.
`kestrel trigger test` prints which agent a firing for an event would choose. A label only matters
when a firing opens a session. Changing an issue's labels later does not change the agent of the
session already working on it.

### One-off triggers

For a trigger you are trying out, the same declaration goes on the command line:

```sh
kestrel trigger declare ready \
  --organization acme \
  --filter '{"all": [
    {"exact": {"source": "https://github.com/jtmthf/kestrel"}},
    {"exact": {"type": "com.github.issues.labeled"}},
    {"exact": {"data.label.name": "ready-for-agent"}}
  ]}' \
  --brief @.kestrel/briefs/ready.md \
  --branch 'kestrel/issue-{{ event.data.issue.number }}' \
  --workspace kestrel \
  --agent builder
```

`--filter` and `--brief` each take their text as it is, from a file as `@path`, or from standard
input as `-`. `--correlation` pairs with `--on-miss`. An apply leaves a trigger declared this way
alone unless its file declares one of the same name, which it then takes over; `kestrel trigger
show` says which way each was declared.

### Strangers

Both ways of declaring a trigger warn, by name, about one whose filter lets in events from people
outside the organization — anything that does not require the `author_association` GitHub reports
to be `OWNER`, `MEMBER` or `COLLABORATOR`, unless the filter rules out GitHub's events altogether.
Until `0.4` there is no policy beneath a run, so such a trigger is an unsupervised agent with your
credentials on your repository, briefed by whatever a stranger wrote. Keep it if that is what you
meant; the warning is there so that it was decided rather than discovered.

Label an issue on that repository `ready-for-agent`, and within a poll interval there is a session
open with a run queued behind it, which nobody asked for:

```sh
kestrel session list --organization acme
```

```
01a07c31-6a10-7cc2-9d41-0b5b6a2b7f04  open  kestrel  builder  01a07c31-4d0c-7b91-88f1-2f1a9c0b3e77
```

The last column is the event that started it. `kestrel session show` prints it beside the branch
the trigger rendered, which the session works on for its whole life; the supervisor cuts that
branch from the workspace's when the repository does not have it yet. The rendered brief is the session's
first transcript entry:

```
1  2026-09-07T14:02:03.118Z  brief  ready  Work https://github.com/jtmthf/kestrel/issues/44: 0.1/21: The GitHub Trigger opens a Session from an Event
2  2026-09-07T14:02:03.118Z  participant joined  builder
```

The agent's first prompt is that brief, character for character, and nothing else. kestrel does not
wrap it, summarise it or read skill syntax in it, so a brief that leads with your harness's own
invocation — `/implement` for Claude Code, `$tdd` for Codex — reaches the harness where it looks for
one. Copying the issue's body into the brief would hand the agent a snapshot; give it the link and
ask it to read the current issue and its comments itself, with `gh`, which the development image
carries. Once anything is said after the brief, the next prompt is the transcript as earlier
context instead.

A brief can also take an instruction supplied with a dispatch, rather than one written into the
trigger. It is the template's `instruction` — `none` when the dispatch gave none — so the template
decides whether it replaces the usual work or adds to it:

```yaml
    brief: |
      {% if instruction %}{{ instruction }}{% else %}/implement{% endif %} {{ event.data.issue.html_url }}

      Read the issue and its comments with `gh issue view --comments` before you start.
```

A brief, branch or correlation that cannot render fails the firing: nothing opens, the control
plane logs why, and no later sweep tries that trigger on that event again. A correlation is held by
the session it opened, and is unique among the organization's open sessions. Events arriving while
that Session has an active Run wait together, then become one transcript entry and one next Run.

A trigger fires at most once per event, so the same label arriving in two overlapping poll windows
opens one session and not two. Taking the label off and putting it back is a new event, and starts
new work.

A trigger also fires only for events recorded after it was declared, so what kestrel already saw on
the repository before you declared it opens nothing, however long that backlog is. That is why the
integration comes first above: its first poll reads a page of what has already happened, and a
trigger declared after that leaves it alone. The same holds for the first apply into a repository
holding a month of events, and for an apply that changes a trigger: the changed trigger matches
only what is recorded from then on, so widening a filter never reaches back for what the narrower
one passed over.

**The event chooses nothing.** The agent, the workspace and the model come from the declaration you
applied; only the data comes from the event. Anyone who can label an issue on a public
repository could otherwise pick which agent's credentials the run gets
([ADR-0013](docs/adr/0013-an-event-supplies-data-never-authority.md)).

`kestrel trigger list --organization acme` shows what each one matches, the way you would say it:

```
01a07c30-9b2e-7f41-a8c3-5d0e1f2a3b4c  ready  enabled  kestrel  builder  source = "https://github.com/jtmthf/kestrel" and type = "com.github.issues.labeled" and data.label.name = "ready-for-agent" and data.issue.author_association = "MEMBER"
```

Before trusting a trigger with work, ask it about an event kestrel already recorded. A test starts
nothing, and it answers for any event in the organization — including one recorded before the
trigger was declared, which the trigger itself will never fire for. It says whether the filter
matches, then prints the branch, the correlation and the brief exactly as that event renders
them:

```sh
kestrel trigger test ready --organization acme --event 01a07c31-4d0c-7b91-88f1-2f1a9c0b3e77
```

```
matches
branch        kestrel/issue-44
correlation   -

Work https://github.com/jtmthf/kestrel/issues/44: 0.1/21: The GitHub Trigger opens a Session from an Event
```

It renders even when the filter does not match, so a brief can be written against the event it is
for before the filter is right. Add `-f .kestrel/triggers.yaml` to test the trigger as the file
declares it, before you apply it, and `--instruction` to render the brief as a dispatch carrying
that instruction would. A template that cannot render fails the test, naming the trigger,
the event, the line of the template that failed, and the variables it had to work with.

An event several triggers match fires every one of them; no trigger is first, and matching one
does not stop the next. Disabling stops one firing without forgetting what it was:

```sh
kestrel trigger disable ready --organization acme
```

```
disabled
```

`kestrel trigger enable ready --organization acme` puts it back.

Each Trigger has a budget of ten firings per hour. The firing that would exceed it is recorded
without opening a Session, and disables only that Trigger. `kestrel trigger show` names the reason;
an operator must explicitly enable it again, which starts its budget afresh.

### A trigger on a schedule

Some work has no event to start it: a weekly dependency sweep, a nightly triage pass. A trigger can
declare a schedule in place of a filter, and not both:

```sh
kestrel trigger declare sweep \
  --organization acme \
  --every 24h \
  --brief 'Sweep the backlog for stale issues as of {{ event.time }}' \
  --branch 'kestrel/sweep-{{ event.id[:10] }}' \
  --workspace kestrel \
  --agent builder
```

Each time the schedule elapses, counting from the declaration, kestrel mints an event of its own and
records it like any other, with no integration: `type` is `dev.kestrel.schedule.elapsed`, `source`
is `urn:kestrel:trigger:<trigger id>`, `id` and `time` are the moment it was due, and `data` holds
the trigger's name and interval. The trigger then fires for it on the same path a matched event
takes, so the brief renders from `event`, the session's first entry is that brief, and the
firing budget applies. A schedule that would exceed the budget is refused when you declare it, so
nothing shorter than six minutes is accepted. Elapsings missed while kestrel was down fire once, not
once each, and a disabled trigger's schedule does not elapse at all. A session opened this way has no
issue to report to, so its outcome goes nowhere.

`trigger test` needs no event for a scheduled trigger. Given none, it renders against the event the
next elapsing would mint, and says when that is due:

```sh
kestrel trigger test sweep --organization acme
```

```
matches
elapsing      2026-09-17T14:02:03.118Z
branch        kestrel/sweep-2026-09-17
correlation   -

Sweep the backlog for stale issues as of 2026-09-17T14:02:03.118Z
```

## The answer comes back to the issue

An integration carries kestrel's requests outbound as well as events inbound, and the one you
registered above declares both. So when the run ends, the issue that started it gets a comment:

```
**kestrel** — run succeeded

> Opened https://github.com/jtmthf/kestrel/pull/92 with the fix and a regression test.

Session `01a07c31-6a10-7cc2-9d41-0b5b6a2b7f04` · run `01a07c33-2f88-7a05-bb31-58c0d9e4d7f0`
```

The quoted part is the last thing the agent said. kestrel reasons about no git and never learns
which pull request was opened — if there is a link there, it is there because the agent named it.
A run that failed gets a comment too, saying so and saying why.

Exactly one comment per run, whatever happens in between. The comment carries a marker naming the
run, so a control plane killed between sending it and hearing back reads the issue on the way up,
recognises its own comment and does not leave a second. A comment GitHub refuses is tried again on
the next sweep and never changes how the run ended.

Register an integration with `--carries inbound` and kestrel watches the repository without ever
writing to it.

## Continue a session

A new comment on the issue that opened a session posts that message to its transcript and enqueues
another run in the same session, on the same instance and checkout. Each run gets a fresh supervisor
and agent runtime. Before its agent starts, the supervisor pages the whole transcript into the
runtime, so the new turn sees the brief, earlier runs, and the follow-up message.

If a run is active when the comment arrives, the message waits durably and one further run is
enqueued when the active one ends. If the session has been sealed, the comment opens a new session
whose `continues` field names the sealed one, on the sealed session's branch.

An operator can post the same kind of message directly:

```sh
kestrel session post 01a07846-49fa-7dc0-a44b-183a63794ee3 "please add the missing test"
```

Pass `--as-participant NAME` to record a name other than `operator` in the transcript.

## Where this stops

Three things you will meet following this document.

**Only GitHub hears back.** A generic webhook or a schedule starts work but has nowhere to say how
it went, no Slack message starts anything, and nothing decides which of several queued runs goes
first.

**A failed run is not retried.** kestrel retries dispatch and never work: a run that started and
failed stays failed.

**You cannot join a session while it runs.** Reading its transcript afterwards is the only way to see
what happened.

[`ROADMAP.md`](ROADMAP.md) is the order the rest arrives in, and the issue tracker carries the
reasoning behind each decision and the objections it survived.
