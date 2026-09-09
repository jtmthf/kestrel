# kestrel declares the branch and learns the pull request

kestrel resolves a session's checkout and declares the branch its runs work on. The agent commits and
pushes. kestrel learns that a pull request exists from the GitHub integration's own event stream,
correlated by the branch it declared. The control plane runs no git command and writes no pull
request body.

`USAGE.md` says kestrel "reasons about no git and never learns which pull request was opened," and
that sentence was load-bearing: it is what keeps the Workspace generic and the compute contract at
six capabilities. It is also why a run can report success having produced nothing, why an operator
cannot see what a run has changed, and why nothing can establish that an instance is safe to reap.
This decision breaks the sentence in the narrowest place that closes all three.

## Considered options

**Observe only** — the supervisor reports what git says, the agent invents the branch and opens the
pull request — was rejected on the branch. A branch name an agent chose is data kestrel received from
a run rather than a declaration a human reviewed, and correlating a pull request back to a run needs
a name kestrel knew in advance. It is the same instinct as
[ADR-0013](0013-an-event-supplies-data-never-authority.md): the thing that identifies the work should
not be something the work made up.

**Own all of it** — kestrel creates the branch, commits, and opens the pull request through the
integration — was rejected because it puts the control plane in the business of writing pull request
bodies, which is the one step where judgement about *what to say* is the whole job. It is also the
industry norm and worth noting as such: Copilot, Codex, Cursor and Claude Code all commit, push and
open pull requests themselves.

**Declare the branch, learn the rest** is what shipped. kestrel already registers a GitHub
integration that polls the repository, so a `pull_request` event naming a branch kestrel itself
declared is a **correlation** — vocabulary kestrel already has, machinery kestrel already runs. No
git command in the control plane, no seventh capability, and no new outbound call.

## Consequences

- **The supervisor reports git state up the link**, alongside what the agent said: the branch, what
  is committed, what is not, what is untracked. This is what
  [ADR-0018](0018-an-instance-lives-until-its-session-seals.md)'s reaping gate reads, and it is why
  that gate can be trusted where an agent's assertion cannot.
- **The branch and the pull request are shared-state transcript entries**, not run detail. A human
  joining a session late needs to know there is a pull request; they do not need the tool call that
  pushed it. See [ADR-0020](0020-the-transcript-records-what-the-runtime-emits-in-kinds.md).
- **`USAGE.md`'s claim is rewritten rather than deleted.** kestrel still reasons about no git: it
  runs no git command, parses no diff of its own, and understands no git semantics. What changed is
  that it now *names* a branch and *receives* facts about one, which is a much smaller claim than the
  one the sentence was defending.
- **The correlation is only as good as the integration.** A pull request opened against a repository
  kestrel has no integration for is never learned, and the session says so rather than guessing. That
  is the honest failure and it degrades to exactly today's behaviour.
