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
it created.

An **organization** is the outermost boundary. Every record kestrel keeps belongs to exactly one:

```sh
kestrel organization declare acme
```

A **workspace** is what a session's work happens against — repositories and a branch. Repeat
`--repository` to name more than one.

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

`kestrel organization list`, `kestrel workspace list --organization acme` and
`kestrel agent list --organization acme` show what you have declared.

## Open a session

A **session** is the durable thread of work. It survives restarts, owns a transcript, and contains
many runs over its life.

```sh
kestrel session open --organization acme --workspace kestrel --agent builder
```

Keep the identifier it prints — everything below takes it.

```sh
kestrel session show 01a07846-49fa-7dc0-a44b-183a63794ee3
```

```
session       01a07846-49fa-7dc0-a44b-183a63794ee3
organization  acme
workspace     kestrel
agent         builder
state         open
opened        2026-09-06T19:51:07.514310886Z
```

It already has a transcript, because opening it put the agent in as a participant:

```sh
kestrel session transcript 01a07846-49fa-7dc0-a44b-183a63794ee3
```

```
1  2026-09-06T19:51:07.514407094Z  participant joined  builder
```

## Enqueue a run

A **run** is one execution of an agent runtime inside one environment. At most one is ever active in
a session.

```sh
kestrel run enqueue --session 01a07846-49fa-7dc0-a44b-183a63794ee3
```

Within seconds the control plane claims it, provisions a container, clones the workspace's
repositories into it, and starts an agent runtime there which dials back over the link.

```sh
kestrel run list --session 01a07846-49fa-7dc0-a44b-183a63794ee3
```

```
01a07846-5d97-7230-9315-bfef2a644006  docker/kestrel-01a07846-5d97-7230-9315-bfef2a644006  -  active
```

The second column is the environment, and the third is the model the run is on, which it says once
the turn is over. The environment is a real container, and the supervisor inside it says what it is
doing:

```sh
docker logs -f kestrel-01a07846-5d97-7230-9315-bfef2a644006
```

```
supervisor started
link open
reported connected
instruction start 1
reported started 1
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

A session is durable from the moment it is opened. An environment is disposable and survives
nothing. Bring the whole stack down and back up to see both:

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

The session and its transcript are intact. The environment is gone, and the run that was executing
in it ended with an explicit status rather than staying active forever.

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
continues     01a07846-49fa-7dc0-a44b-183a63794ee3
```

## Where this stops

Four things you will meet following this document.

**Nothing carries a task to a run.** Every run asks its agent the same fixed question, so the work is
undirected. This is the one that matters — everything above is machinery waiting for it.

**Nothing triggers or schedules a run.** Every session here was opened by hand. No GitHub issue,
Slack message, webhook or schedule starts one, and nothing decides which of several queued runs goes
first.

**A failed run is not retried.** kestrel retries dispatch and never work: a run that started and
failed stays failed.

**You cannot join a session while it runs.** Reading its transcript afterwards is the only way to see
what happened.

[`ROADMAP.md`](ROADMAP.md) is the order the rest arrives in, and the issue tracker carries the
reasoning behind each decision and the objections it survived.
