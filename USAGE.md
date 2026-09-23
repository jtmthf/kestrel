# Using kestrel

This walks from an empty machine to a run — kestrel provisioning an isolated container, cloning a
repository into it, and driving a coding agent there. Everything below works today. What does not
work yet is telling that agent what to do, which is the whole of
[Where this stops](#where-this-stops).

kestrel is at rung `0.1`. [`ROADMAP.md`](ROADMAP.md) is the order the rest arrives in.

## Before you start

- **Docker**, with Compose v2. kestrel asks for no configuration of its own, and there is nothing
  for you to supply.
- **An amd64 or arm64 machine.** The image a run executes in is built for those two, and the build
  fails on anything else rather than producing something that will not start.
- **A clone of this repository.** The stack builds from source rather than pulling images.
- **Rust**, through [rustup](https://rustup.rs), to build the `kestrel` Client. The toolchain
  `rust-toolchain.toml` pins is fetched on the first build.

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

Every command below is the **Client**, `kestrel`, a program of its own that reaches the control
plane over HTTP and holds nothing itself. Install it from the same clone:

```sh
cargo install --locked --path crates/kestrel-client
```

The stack publishes the control plane's operator boundary on your machine's loopback, at
`127.0.0.1:7718`, which is where the Client looks unless `--control-plane` or
`KESTREL_CONTROL_PLANE` names another URL. Nothing there authenticates the caller, so the port is
published on loopback and nowhere else; reach a control plane on another machine through a tunnel.
`kestrel status` says which control plane it reached, what exists there, and what to run next.

## Declare what the work happens against

Three declarations, in this order, because each needs the one before it. Each prints the identifier
of what it declared, and nothing else, so `$(…)` captures it. A declaration describes what should
exist, so running one again is safe: an unchanged one changes nothing, and a changed one updates the
workspace or agent by that name in place, keeping its identifier.

An **organization** is the outermost boundary. Every record kestrel keeps belongs to exactly one:

```sh
kestrel organization declare acme
```

With one organization, every command below applies to it without being told. Once there are two,
each needs `--organization`, `KESTREL_ORGANIZATION`, or a committed `.kestrel/organization` naming
one, and refuses to guess.

A **workspace** is what a session's work happens against — repositories and the base branch each
session's own branch is cut from. Repeat `--repository` to name more than one.

```sh
kestrel workspace declare kestrel \
  --repository https://github.com/jtmthf/kestrel \
  --branch main
```

An **agent** is a configured identity rather than a running process: the agent runtime that drives
it, and the model it works with.

```sh
kestrel agent declare builder
```

Naming no `--model` asks for whatever the agent runtime's own default is, which is what you want
here; the run records which model that turned out to be. Name a specific model and the run fails at
model selection unless that runtime offers it.

Changing an agent's model is configuration rather than a rebuild, and a run already in flight stays
on the model it was dispatched with:

```sh
kestrel agent model builder --model anthropic/claude-opus-4-5
```

Once a run has worked, kestrel knows what that runtime advertised, and refuses a model outside it
where you declare it rather than where it would be dispatched.

`--runtime` names the agent runtime: `opencode` unless you say otherwise, or `claude` or `codex`.
The work role maps each name to the command an environment spawns and speaks ACP to, which by
default is `opencode acp --print-logs`, `claude-agent-acp` and `codex-acp`; the `kestrel-dev` image
carries all three. Set `KESTREL_AGENT_RUNTIME` on the control plane, or pass
`--agent-runtime NAME=COMMAND` repeatedly, to change the table. A run whose agent names a runtime
missing from it fails and says which.

```sh
kestrel agent declare codex --runtime codex
```

A session takes its agent's runtime and model when it opens and keeps them while it is open:
redeclaring the agent, or changing its model, changes the sessions opened after that.

`kestrel organization list`, `kestrel workspace list` and `kestrel agent list` show what you have
declared.

## Open a session

A **session** is the durable thread of work. It survives restarts, owns a transcript, and contains
many runs over its life.

```sh
kestrel session open --workspace kestrel --agent builder
```

It prints the session's identifier. Everything below takes a session as that identifier, as any
prefix of it that names only one, as the generated name `session show` prints, or as `latest` for
the one opened most recently.

The session fixes the workspace's repositories as they are now and declares a branch of its own,
`kestrel/<session>`, so sessions opened side by side never work on one another's branch. Pass
`--branch` to work on an existing branch instead. Before the agent starts, the supervisor on the
session's instance clones each repository and checks that branch out, cutting it from the
workspace's when the repository does not have it yet; a checkout that fails ends the run naming the
repository and the branch. A later run on the same instance finds the checkout exactly as the run
before it left it, pushed or not. The control plane itself runs no git.

```sh
kestrel session show latest
```

```
id              01a07846-49fa-7dc0-a44b-183a63794ee3
name            grand-acorn-simpjvvl
organization    acme
workspace       kestrel
agent           builder
profile         -
base            main
branch          kestrel/01a07846-49fa-7dc0-a44b-183a63794ee3
instance        -
held            -
correlation     -
state           open
opened at       2026-09-06T19:51:07.514310Z
last active at  2026-09-06T19:51:07.514310Z
sealed at       -
started by      -
continues       -
continued by    -
```

On a terminal the Client prints for a person to read. Piped anywhere else it prints the same fields
tab-delimited, one record a line; `--json id,state` prints just those fields as JSON, one record a
line, whatever it is attached to.

It already has a transcript, because opening it put the agent in as a participant:

```sh
kestrel session transcript latest
```

```
1  2026-09-06T19:51:07.514407Z  {"kind":"participant_joined","participant":"builder"}
cursor  01a07846-49fa-7dc0-a44b-183a63794ee3:1
```

## Enqueue a run

A **run** is one execution of an agent runtime on its session's instance: one conversation with it,
over as many turns as the session gives it. At most one is ever open in a session.

```sh
kestrel run enqueue --session latest
```

Within seconds the control plane claims it, provisions a container for the session, and starts a
supervisor in it that clones the workspace's repositories and spawns an agent runtime, dialling back
over the link. The container is the session's **instance**: every later run in the session starts a
supervisor of its own in the same one.

The work role keeps up to two runs working at once by default. That conservative default leaves
room on a laptop for two agent runtimes mid-turn. Set `KESTREL_MAX_ACTIVE_RUNS` on the control-plane
container, or pass `--max-active-runs RUNS`, to choose a different positive limit. Only a run getting
to its first turn or mid-turn counts against it: a run waiting between turns keeps its agent
conversation and instance but frees its place, so another session can work meanwhile. Queued runs
and follow-ups for waiting runs take a freed place in the order they arrived.

```sh
kestrel run list --session latest
```

```
id                                    name                  state   waiting  status  because  instance                                             worked model
01a07846-5d97-7230-9315-bfef2a644006  eager-raven-owvigqbk  active  false    -       -        docker/kestrel-01a07846-5d97-7230-9315-bfef2a644006  -
```

`waiting` is whether the agent has answered its turn and waits for the next one, and the worked model
is the one the run is on, which it says once the turn is over. The instance is a real container, and the run's supervisor in it says what it is
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

Each line the agent runtime writes to stderr joins them as it is written, named for its run and
never in the transcript: it is the runtime's own diagnostics, not the agent speaking. opencode is
spawned with `--print-logs`, so its log is there by default; raise its level through the runtime
table, as `--agent-runtime 'opencode=opencode acp --print-logs --log-level DEBUG'`. A line longer
than 4 KiB is cut short and says so.

```
INFO kestrel::work: its agent runtime wrote to stderr run=01a07846-5d97-7230-9315-bfef2a644006 line="timestamp=2026-09-21T22:16:20.783Z level=INFO run=304e054b message=init"
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

### Run on your own subscription

A **subscription profile** is one person's login to a subscribed runtime: Codex through a ChatGPT
plan, Claude Code through a Claude plan, or an OpenCode Go key. kestrel keeps it, so no image,
checkout or provider account has to. It reaches only the runs of sessions that name it.

```sh
kestrel profile declare jack --owner jack
kestrel profile set jack --file .codex/auth.json < ~/.codex/auth.json
kestrel profile set jack --file .local/share/opencode/auth.json < ~/.local/share/opencode/auth.json
kestrel profile set jack --variable CLAUDE_CODE_OAUTH_TOKEN
```

The last command waits for the token `claude setup-token` printed: paste it and press Ctrl-D. Each login is read from
standard input, sealed with the key beside the database, and never printed again. `kestrel profile list` shows a profile's owner and what it holds, by name only. A `--variable`
goes into the agent runtime's environment. A `--file` is written at that path beneath the agent's
home when the run starts. When the run ends it is read back and removed from the instance, so a
login the runtime refreshed there is the one the next run gets, on this instance or a fresh one. A
profile belongs to the owner it was declared with, and redeclaring it under another owner is
refused.

Codex writes `auth.json` only when `cli_auth_credentials_store = "file"` is set where you log in.
Claude Code keeps a macOS login in the Keychain rather than a file, which is why its token is held
as a variable. Whether a Claude plan may be used through its ACP adapter is unsettled
([ADR-0025](docs/adr/0025-subscription-profiles-are-personal.md)).

Name the profile when you open the session, or give a trigger's declaration `profile: jack`:

```sh
kestrel session open --workspace kestrel --agent codex --profile jack
```

A session that names a profile needs no provider credential, and a follow-up that continues it keeps
the profile. The operator boundary authenticates nobody, so naming a profile is its owner's
authorization. Review a trigger that names one as carefully as one that admits strangers.

Codex rotates its login as it refreshes it, and two copies refreshing at once can revoke each other.
The work role therefore runs one Codex run per profile at a time and leaves the others queued.
`--serialized-runtime` (`KESTREL_SERIALIZED_RUNTIME`) names the runtimes handled this way, and
defaults to `codex`.

## Your sessions survive a restart

A session is durable from the moment it is opened. A run in flight is not. Bring the whole stack
down and back up to see both:

```sh
docker compose down
docker compose up -d
kestrel session transcript latest
```

```
1  2026-09-06T19:51:07.514407Z  {"kind":"participant_joined","participant":"builder"}
2  2026-09-06T19:51:13.316822Z  {"kind":"run_started","run":"01a07846-5d97-7230-9315-bfef2a644006"}
3  2026-09-06T19:58:58.489250Z  {"kind":"run_ended","run":"01a07846-5d97-7230-9315-bfef2a644006","exit":{"status":"failed","because":"the control plane stopped while this run was in flight"}}
cursor  01a07846-49fa-7dc0-a44b-183a63794ee3:3
```

The session and its transcript are intact. The run that was executing ended with an explicit status
rather than staying active forever, and its supervisor was stopped. The session's instance is still
there, with the checkout as the run left it, for the session's next run.

The instance lives until the session seals, and longer if it may hold the only copy of some work;
see [Sealing a session](#sealing-a-session). If an instance is gone when a run needs it, that run fails and says that whatever the
instance held that was never pushed is lost; the next run provisions a fresh instance and checks the
session's branch out from the remote.

`docker compose down --volumes` removes the named volume too, and with it every session, transcript
and declaration on this machine. It is the only command here that destroys anything.

## Following a transcript

A transcript is streamed rather than paged: `session transcript` prints every entry there is and
stops. The cursor it ends on is written to **stderr**, so it never runs together with the entries on
stdout:

```
cursor  01a07846-49fa-7dc0-a44b-183a63794ee3:2
```

Pass it back to read only what came after it:

```sh
kestrel session transcript latest --cursor 01a07846-49fa-7dc0-a44b-183a63794ee3:2
```

```
3  2026-09-06T19:58:58.489250Z  {"kind":"run_ended","run":"01a07846-5d97-7230-9315-bfef2a644006","exit":{"status":"failed","because":"the control plane stopped while this run was in flight"}}
cursor  01a07846-49fa-7dc0-a44b-183a63794ee3:3
```

Add `--follow` to keep printing entries as they are appended, until the session seals. A follow that
loses the control plane reconnects from the last entry it printed, and repeats none.

## Sealing a session

Sealing ends a session without deleting it. A sealed session stays readable and is never reopened.

```sh
kestrel session seal latest
```

Sealing archives the session's instance: the work role destroys its container. It does so only when
the last run on it reported a checkout that the remote can restore, with nothing untracked,
uncommitted or stashed and no commit that no remote branch has. Output that git ignores, such as a
`target/` directory, does not count. Anything else holds the instance, and the seal is refused. The run
above was cut off before its supervisor could say what the checkout held, so this session is refused:

```
Error: the control plane refused: the session 01a07846-49fa-7dc0-a44b-183a63794ee3's instance docker/kestrel-01a07846-5d97-7230-9315-bfef2a644006 may hold the only copy of its work (no run reported what its checkout holds); publish it from a follow-up run, or release the instance to discard it
```

Every held instance is listed with its reason, and `kestrel session show` repeats the reason on its
`held` line:

```sh
kestrel instance list
```

```
session                               instance                                             because
01a07846-49fa-7dc0-a44b-183a63794ee3  docker/kestrel-01a07846-5d97-7230-9315-bfef2a644006  no run reported what its checkout holds
```

A reason read from git names the repository, the branch and what it found, such as
`https://github.com/jtmthf/kestrel on kestrel/01a07846-… has 2 unpushed commits, 1 untracked file`.
To keep that work, post a message asking the agent to push it. To discard it, release the instance.
The release destroys the instance and is recorded in the session's transcript:

```sh
kestrel instance release latest
kestrel session seal latest
```

A sealed session accepts no further runs:

```
Error: the control plane refused: the session 01a07846-49fa-7dc0-a44b-183a63794ee3 is sealed, and accepts no run
```

Sealing ends a run that is waiting between turns, and it succeeds. A session whose run is still in
a turn, or still queued, refuses to seal until that turn is answered.

A session seals itself too. `last active` moves when the session opens, when a run is enqueued into
it, when one of its runs answers a turn, and when one ends; a session that has sat at the same
`last active` for 24 hours with no turn in flight is sealed by kestrel, exactly as the command above
would have. A session whose instance is held stays open, however long it has been idle, until its
work is pushed or its instance released.

Work that would have continued it starts a new session that records the sealed one:

```sh
kestrel session open --workspace kestrel --agent builder --continues latest
```

Both ends of that link are visible. The new session shows what it continues, and the sealed one
names it on its `continued by` line:

```sh
kestrel session show latest --json id,state,continues
```

```
{"id":"01a0784e-7844-7ee3-a273-9f032047aeb4","state":"open","continues":"01a07846-49fa-7dc0-a44b-183a63794ee3"}
```

## Hand kestrel an issue

Every session above you opened by hand. A **trigger** is the standing rule that opens one for you:
what it matches, and the agent and workspace it starts that work with.

kestrel has to be able to see the repository first. An **integration** is a credentialed connection
to an external system, and it declares which directions it carries — events inbound, kestrel's
requests outbound, or both:

```sh
kestrel integration register github origin \
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
kestrel integration register webhook ci --secret "$KESTREL_WEBHOOK_SECRET"
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
kestrel event list
```

```
record                                time                  source                             type                             subject
01a07c31-4d0c-7b91-88f1-2f1a9c0b3e77  2026-09-07T14:01:58Z  https://github.com/jtmthf/kestrel  com.github.issue_comment.created  #44
```

`kestrel event show <record>` prints one event whole, its payload included.

Now the rule itself. A trigger decides what an agent does to a repository with your
organization's credentials, so declare it in a file you keep in version control and review in a
pull request. kestrel's own is `.kestrel/triggers.yaml`, and it starts work only when someone hands
the work over on purpose: its maintainer opening a comment with `@kestrel`, or an operator
dispatching an issue.

```yaml
triggers:
  delegated:
    filter:
      all:
        - exact: {source: "https://github.com/jtmthf/kestrel"}
        - exact: {type: com.github.issue_comment.created}
        - any:
            - all:
                - exact: {data.user.login: jtmthf}
                - prefix: {data.body: "@kestrel"}
            - all:
                - exact: {data.comment.user.login: jtmthf}
                - prefix: {data.comment.body: "@kestrel"}
    brief: |
      {% if instruction %}{{ instruction }}{% else %}/implement{% endif %} {{ event.source }}/issues/{{ event.subject | replace("#", "") }}

      Read the issue and its comments with `gh issue view --comments` before you start.
    branch: kestrel/issue-{{ event.subject | replace("#", "") }}
    correlation: "{{ event.source }}{{ event.subject }}"
    on_miss: open
    workspace: kestrel
    agent: builder
    allows: [codex, claude]
```

The filter names the one person allowed to start work, twice over: a poll reports a comment's author
as `user`, while a webhook nests the comment. A label, `ready-for-agent` included, starts nothing: it
says what state an issue is in, never that anyone handed it over. An ordinary comment starts nothing
either, and neither does anything someone else says. Assigning an issue starts nothing: kestrel is
meant to reach GitHub as an App, and GitHub does not let an issue be assigned to one.

kestrel cannot read that file out of the repository for itself: reading it takes a checkout, a
checkout takes a session, and a session takes a trigger. So you apply it, and kestrel prints the
diff it makes:

```sh
kestrel trigger apply -f .kestrel/triggers.yaml
```

```
+ delegated
    matches
      + source = "https://github.com/jtmthf/kestrel" and type = "com.github.issue_comment.created" and ((data.user.login = "jtmthf" and data.body starts with "@kestrel") or (data.comment.user.login = "jtmthf" and data.comment.body starts with "@kestrel"))
    workspace
      + kestrel
    agent
      + builder
    allows
      + claude, codex
    branch
      + kestrel/issue-{{ event.subject | replace("#", "") }}
    correlation
      + {{ event.source }}{{ event.subject }}
    on miss
      + open
    brief
      + {% if instruction %}{{ instruction }}{% else %}/implement{% endif %} {{ event.source }}/issues/{{ event.subject | replace("#", "") }}
      +
      + Read the issue and its comments with `gh issue view --comments` before you start.
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
work.

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
  --filter '{"all": [
    {"exact": {"source": "https://github.com/jtmthf/kestrel"}},
    {"exact": {"type": "com.github.issue_comment.created"}},
    {"exact": {"data.user.login": "jtmthf"}},
    {"prefix": {"data.body": "@kestrel ready"}}
  ]}' \
  --brief @.kestrel/briefs/ready.md \
  --branch 'kestrel/issue-{{ event.data.issue.number }}' \
  --workspace kestrel \
  --agent builder
```

`--filter` and `--brief` each take their text as it is, from a file as `@path`, or from standard
input as `-`. `--correlation` pairs with `--on-miss`. An apply leaves a trigger declared this way
alone unless its file declares one of the same name, which it then takes over; `kestrel trigger
show` says which way each was declared, on its `applied` line.

### Strangers

Both ways of declaring a trigger warn, by name, about one whose filter lets in events from people
outside the organization — anything that does not require the `author_association` GitHub reports
to be `OWNER`, `MEMBER` or `COLLABORATOR`, or name the one login allowed to act, unless the filter
rules out GitHub's events altogether.
Until `0.4` there is no policy beneath a run, so such a trigger is an unsupervised agent with your
credentials on your repository, briefed by whatever a stranger wrote. Keep it if that is what you
meant; the warning is there so that it was decided rather than discovered.

**The image a run needs to open a pull request.** The image a run executes in is `kestrel-env`
unless you say otherwise, and it carries only the supervisor, opencode and git. It has no `gh`, so
an agent working inside it cannot open a pull request however plainly the brief asks it to; the
outcome comment would arrive without one.

[`kestrel-dev`](images/kestrel-dev/README.md) is the image this repository ships for work that has to
touch GitHub. It derives from `kestrel-env` and adds `gh`, Rust, and the Claude Code and Codex agent
runtimes. Build it, then point the control plane at it with a gitignored `compose.override.yaml`,
which Compose merges automatically:

```sh
docker compose build kestrel-env
docker build --file images/kestrel-dev/Dockerfile --tag kestrel-dev .
```

```yaml
services:
  kestrel:
    environment:
      KESTREL_IMAGE: kestrel-dev
```

```sh
docker compose up -d
```

`KESTREL_IMAGE` names the image the Docker driver provisions an Instance from. It is not
`KESTREL_ENV_IMAGE`, which is the tag Compose builds the base image under, so pointing that at
`kestrel-dev` would only relabel the image Compose rebuilds. The named volume survives the recreate,
so every declaration and session above is still there.

`gh` reads its token from its own environment, so a run needs one there. Name the credential for that
variable and hand it the token `gh` already holds:

```sh
gh auth token | kestrel credential set GH_TOKEN
```

The secret travels on standard input rather than in an argument. `GH_TOKEN` is not a name kestrel treats
specially: it is simply the variable `gh` already looks for, and kestrel hands it to the agent's
process the way it hands over a Provider Credential
([ADR-0010](docs/adr/0010-a-provider-credential-crosses-the-link-at-the-spawn.md)). `repo` scope opens
pull requests and merges them, and `gh` merges with the credential it opened with; kestrel hands the
credential over and stays out of what the run does with it. A human merge gate has to come from
outside the credential: a required review gates only when the reviewer is a GitHub identity other than
the one the token acts as, because GitHub will not let a pull request's author approve it — which is
why this repository's own CI gates its merges instead
([ADR-0027](docs/adr/0027-ci-gates-the-merge-and-a-queue-lands-the-batch.md)). A narrower token is no
answer either: merging writes to the base branch and needs the contents-write authority that pushing
the pull request's branch already needs, so a fine-grained token that can push the branch can merge
it.

Comment `@kestrel` on an issue in that repository, and within a poll interval there is a session
open with a run queued behind it:

```sh
kestrel session list
```

```
id                                    name                  state  workspace  agent    started by
01a07c31-6a10-7cc2-9d41-0b5b6a2b7f04  brisk-heron-kqpzmwdt  open   kestrel    builder  01a07c31-4d0c-7b91-88f1-2f1a9c0b3e77
```

The last column is the event that started it. `kestrel session show` prints it beside the branch
the trigger rendered, which the session works on for its whole life; the supervisor cuts that
branch from the workspace's when the repository does not have it yet. The rendered brief is the session's
first transcript entry:

```
1  2026-09-07T14:02:03.118Z  {"kind":"brief","trigger":"delegated","brief":"/implement https://github.com/jtmthf/kestrel/issues/44\n\nRead the issue and its comments with `gh issue view --comments` before you start."}
2  2026-09-07T14:02:03.118Z  {"kind":"participant_joined","participant":"builder"}
```

The agent's first prompt is that brief, character for character, and nothing else. kestrel does not
wrap it, summarise it or read skill syntax in it, so a brief that leads with your harness's own
invocation — `/implement` for Claude Code, `$tdd` for Codex — reaches the harness where it looks for
one. Copying the issue's body into the brief would hand the agent a snapshot; give it the link and
ask it to read the current issue and its comments itself, with `gh`, which `kestrel-dev` carries. Once anything is said after the brief, the next prompt is the transcript as earlier
context instead.

A brief can also take an instruction supplied when the work is handed over, rather than one written
into the trigger. It is the template's `instruction` — `none` when nobody gave one — so the template
decides whether it replaces the usual work or adds to it, as the `{% if instruction %}` above does.

### Commanding kestrel from a comment

A comment that opens with `@kestrel` is a command. What follows the mention is its instruction, and
an `agent=<name>` straight after the mention chooses the agent, from those the trigger allows, in
place of the trigger's own or one an `agent:` label chooses:

```
@kestrel agent=codex $tdd the date parser, then open a pull request
```

A mention anywhere but the start of a comment commands nothing. Whether a command starts work is the
trigger's filter to say, so the one above obeys only `jtmthf`. A comment kestrel itself left carries
its marker and is never heard as anything. On an issue whose session is open, the command feeds that
session, as a label on it would; on one whose session has sealed, it opens a new session continuing
the sealed one.

### Dispatching an issue

An operator can hand a trigger an issue directly, whether or not anything on GitHub would match it:

```sh
kestrel trigger dispatch delegated --integration origin --issue 44 \
  --instruction '/implement' --agent codex
```

```
outcome      opened
session      01a07c31-6a10-7cc2-9d41-0b5b6a2b7f04
run          01a07c31-6a11-7cc2-9d41-0b5b6a2b7f05
event        01a07c31-6a0f-7cc2-9d41-0b5b6a2b7f03
correlation  -
```

kestrel reads the issue through the integration, records a `dev.kestrel.dispatched` event whose
`data.issue` is the issue as GitHub reports it, and fires only the trigger you named for it: no other
trigger ever fires for a dispatch. From there it is a firing like any other, so a correlation feeds
an open session rather than opening a second one, the firing budget still counts it, and the outcome
goes back to the issue. `--instruction` is the brief's `instruction`, and `--agent` chooses among
the agents the trigger allows. `--instruction` also takes `@FILE` and `-`.

A brief, branch or correlation that cannot render fails the firing: nothing opens, the control
plane logs why, and no later sweep tries that trigger on that event again. A correlation is held by
the session it opened, and is unique among the organization's open sessions. Events arriving while
that Session has an active Run wait together, then become one transcript entry and one next Run.

A trigger fires at most once per event, so the same command arriving in two overlapping poll
windows opens one session and not two. A second command is a new event, and so is a second
dispatch; with a correlation, either feeds the session already open for the issue rather than
opening another.

A trigger also fires only for events recorded after it was declared, so what kestrel already saw on
the repository before you declared it opens nothing, however long that backlog is. That is why the
integration comes first above: its first poll reads a page of what has already happened, and a
trigger declared after that leaves it alone. The same holds for the first apply into a repository
holding a month of events, and for an apply that changes a trigger: the changed trigger matches
only what is recorded from then on, so widening a filter never reaches back for what the narrower
one passed over.

**The event chooses nothing.** The agent, the workspace and the model come from the declaration you
applied; only the data comes from the event. A label or an `agent=` in a command only chooses among
agents the declaration allows. Anyone who can label an issue on a public repository could otherwise
pick which agent's credentials the run gets
([ADR-0013](docs/adr/0013-an-event-supplies-data-never-authority.md)).

`kestrel trigger list` shows each one, and what it matches:

```
id                                    name       state    workspace  agent    every  cron  filter
01a0b47c-6453-7450-a970-c567e92bf109  delegated  enabled  kestrel    builder  -      -     {"all":[{"exact":{"source":"https://github.com/jtmthf/kestrel"}},…]}
```

Before trusting a trigger with work, ask it about an event kestrel already recorded. A test starts
nothing, and it answers for any event in the organization — including one recorded before the
trigger was declared, which the trigger itself will never fire for. It says whether the filter
matches, then prints the branch, the correlation and the brief exactly as that event renders
them:

```sh
kestrel trigger test delegated --event 01a07c31-4d0c-7b91-88f1-2f1a9c0b3e77
```

```
matches      true
elapsing     -
agent        builder
branch       kestrel/issue-44
correlation  https://github.com/jtmthf/kestrel#44
brief        /implement https://github.com/jtmthf/kestrel/issues/44

             Read the issue and its comments with `gh issue view --comments` before you start.
```

It renders even when the filter does not match, so a brief can be written against the event it is
for before the filter is right. Add `-f .kestrel/triggers.yaml` to test the trigger as the file
declares it, before you apply it, and `--instruction` to render the brief as a dispatch carrying
that instruction would. Tested against a command, it renders the command's own instruction and
names the agent the command asks for. A template that cannot render fails the test, naming the trigger,
the event, the line of the template that failed, and the variables it had to work with.

An event several triggers match fires every one of them; no trigger is first, and matching one
does not stop the next. Disabling stops one firing without forgetting what it was:

```sh
kestrel trigger disable delegated
```

```
disabled:operator
```

`kestrel trigger enable delegated` puts it back.

Each Trigger has a budget of ten firings per hour. The firing that would exceed it is recorded
without opening a Session, and disables only that Trigger. `kestrel trigger show` names the reason;
an operator must explicitly enable it again, which starts its budget afresh.

### A trigger on a schedule

Some work has no event to start it: a weekly dependency sweep, a nightly triage pass. A trigger can
declare a schedule in place of a filter, and not both:

```sh
kestrel trigger declare sweep \
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

An interval drifts with whenever it was declared and cannot skip a weekend. Work that belongs at a
time of day, or on certain days, declares a cron expression and the time zone it is read in in place
of `--every`:

```sh
kestrel trigger declare triage \
  --cron '0 9 * * 1-5' \
  --zone America/New_York \
  --brief 'Triage what arrived since yesterday, as of {{ event.time }}' \
  --workspace kestrel \
  --agent builder
```

The expression is five fields — minute, hour, day of the month, month and day of the week (0 is
Sunday) — each `*`, a number, a range such as `1-5`, a list such as `0,30`, or a step over `*` or a
range such as `*/15`. There are no names, no `?`, `L` or `W`, and an expression restricts the day of
the month or the day of the week, never both. The zone is required; `UTC` is one. A time the clocks
spring past elapses at the moment they jump, and a time they fall back over elapses on its first
pass, so a daily trigger fires once each day across a change. It elapses on the same path an
interval does, with `data` holding the expression and the zone in place of the interval, and an
expression whose closest two times are nearer than six minutes is refused the way a short interval
is. `trigger show` prints the expression and its zone, `trigger list` the expression.

`trigger test` needs no event for a scheduled trigger. Given none, it renders against the event the
next elapsing would mint, and says when that is due:

```sh
kestrel trigger test sweep
```

```
matches      true
elapsing     2026-09-17T14:02:03.118Z
agent        builder
branch       kestrel/sweep-2026-09-17
correlation  -
brief        Sweep the backlog for stale issues as of 2026-09-17T14:02:03.118Z
```

## The answer comes back to the issue

An integration carries kestrel's requests outbound as well as events inbound, and the one you
registered above declares both. Each completed turn of a run posts the agent's answer on the issue
that started it, promptly, before the run is over:

```
Opened https://github.com/jtmthf/kestrel/pull/92 with the fix and a regression test.

<!-- kestrel run 01a07c33-2f88-7a05-bb31-58c0d9e4d7f0 turn 1 -->
```

The pull request is the agent's own, opened with the `gh` its run carries and the `GH_TOKEN` set
above; kestrel reasons about no git and never learns which pull request was opened — if there is a
link there, it is there because the agent named it.

A run whose turns already said their answers adds nothing by saying it succeeded, so those turns are
all the issue gets. A run that failed says so, and says why, and a run that answered no turn at all
still says how it ended; that comment names the run and quotes the last thing the agent said:

```
**kestrel** — run failed: the environment could not be provisioned

Session `01a07c31-6a10-7cc2-9d41-0b5b6a2b7f04` · run `01a07c33-2f88-7a05-bb31-58c0d9e4d7f0`
```

Every comment carries an invisible marker naming the run and, for a turn, the turn, so a control
plane killed between sending it and hearing back reads the issue on the way up, recognises its own
comments and does not leave duplicates. A comment GitHub refuses is tried again on the next sweep
and never changes how the run ended.

Register an integration with `--carries inbound` and kestrel watches the repository without ever
writing to it.

## Continue a session

A run is one conversation with its agent, and answering a turn does not end it: nothing the agent
says, and no pull request it opens, does. A new comment on the issue that opened a session posts
that message to its transcript and sends it to the session's open run as its next turn, in the same
agent conversation, on the same supervisor and instance.

If the agent is still working on a turn when the comment arrives, the message waits durably. Every
message that arrived during the turn becomes the next one, in the order they arrived, once the agent
answers. Only a comment from someone the trigger that opened the session authorizes feeds it: a
trigger that names its author takes only that author's remarks, while one that admits outsiders
takes anyone's. A comment on an issue whose session has sealed starts nothing: only a command does,
opening a new session whose `continues` field names the sealed one, on the sealed session's branch. A
command is never also posted as a message.

A run ends when you stop it, when its session seals, or when it fails:

```sh
kestrel run stop 01a07846-5d97-7230-9315-bfef2a644006
```

```
succeeded
```

A run stopped between turns succeeds; one stopped mid-turn, or before it started, fails. A comment on
a session with no open run enqueues a new run in it, on the same instance and checkout, with a fresh
supervisor and agent runtime. Before its agent starts, the supervisor pages the whole transcript into
the runtime, so the new conversation sees the brief, earlier runs, and the follow-up message.

An operator can post the same kind of message directly:

```sh
kestrel session post latest "please add the missing test"
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
