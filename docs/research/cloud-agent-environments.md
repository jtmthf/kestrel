# How cloud coding-agent platforms model the execution environment

Research pass across eleven platforms plus the Dev Container specification, gathered September 2026
from primary documentation only (official docs, specs, and changelogs; no third-party blogs except
where quoted material is itself a verbatim reproduction of an official page). Each platform section
cites the pages read. Where a platform's docs are silent on a question, that is stated explicitly
rather than inferred.

## Findings

**Every platform in scope names a declared configuration noun that is not the running instance**,
and every one of them keeps that noun's scope wider than a single run: an Ona *environment class*,
a Gitpod Flex *environment* selecting one of several *instance types*, a GitHub Actions *runner*
(with *labels*) that Copilot's coding agent inherits wholesale, a Codex cloud *environment* wrapping
the `codex-universal` image, a Cursor `.cursor/environment.json`, a Devin *snapshot* ("golden
snapshot" at the enterprise level), a Jules per-repo *environment* with an optional saved snapshot,
Claude Code's *cloud environment*, a Daytona *Snapshot*, an E2B *template*, a Modal *Image*, and a
Replit *checkpoint* (closer to a save-state than a template, see below). None of the docs describe
the running instance and the declared noun as the same object; all treat construction (build an
image, resolve a class, snapshot a filesystem) as a step logically and temporally prior to a run
that consumes it. The two exceptions worth flagging: Replit's environment is implicit — the
"Repl" itself, with no separate declared class — and GitHub-hosted Actions runners for Copilot are
themselves the noun (a runner *is* the environment; there's no further indirection to an image).

**What the declared noun holds is consistently: base image or Dockerfile, a size/resource tier, a
setup or install script, environment variables/secrets, and (increasingly) a network access
policy.** The image and the resource tier are close to universal. The setup-script mechanism is the
single most common customization path — Codex's setup script, Cursor's `install` field, Claude
Code's setup script, Jules's setup script, Devin's environment YAML blueprint's `initialize`/
`maintenance` steps, Ona's Tasks — all occupy the same slot: a script the platform runs once (or on
cache invalidation) to produce a warm, reusable state, distinct from anything that runs per-turn.
Network policy as a first-class field of the declared noun is explicit in Codex (internet access
off by default, escalatable), Claude Code (four access levels plus a domain allowlist), Copilot
(configurable/disable firewall), and GitHub Actions self-hosted runners (implicitly, via network
placement) — it is not documented as configurable on Devin, Jules, Cursor, or Daytona at the level
of the declared noun.

**Selection is per-repository almost everywhere it is documented**, not per-agent or globally, though
several platforms layer a fallback chain above that: Cursor resolves `.cursor/environment.json` in
the repo, then a personal saved environment, then a team one; Claude Code resolves an explicit pick,
then an organization default, then the first non-bridge environment in the account; Ona's project
selects an environment *class*, and the runner independently maps that class to one of several
compatible *instance types*, so the class is the per-project selection and the instance type is an
infrastructure-level fallback the project never sees. Only Devin is documented as single-tenant at
a courser grain than per-repo: "each organization has exactly one active snapshot," built from every
configured repo's setup steps concatenated together, so Devin's declared noun is scoped to the
organization, not the repository, and a repo cannot have a different snapshot from its siblings.
None of the eleven platforms document per-task or per-dispatch dynamic selection of the environment
in the sense of a caller picking a class value at run start based on run-specific data (e.g., "use
the GPU class only for this run") — where multiple classes exist, the docs describe a human choosing
one in advance (a UI selector, a config file), not the scheduler choosing one per event.

**Lifetime splits into a template layer that is always warm-by-design and a runtime layer whose
warmth is explicitly a caching optimization, not a persistence guarantee.** The template (image,
snapshot, class) persists indefinitely until explicitly rebuilt or invalidated. The *running*
compute is uniformly documented as reclaimable on idle, with the reclaim policy varying: Gitpod
Classic workspaces stop after 30 minutes of inactivity by default (configurable up to 24 hours);
Ona/Gitpod Flex exposes an `--inactivity-timeout` CLI flag and an account-level default auto-stop
preference; Claude Code cloud sessions are marked "expired" after a period of inactivity and their
VM reclaimed; GitHub Actions runners for Copilot are ephemeral per job by construction (a fresh VM
per Actions job); Codex, Cursor, and Daytona instead describe the warm layer as a *filesystem
snapshot cache* with an expiry window (Codex: "up to 12 hours"; Cursor: no stated TTL, invalidated
on Dockerfile/install change) that is rebuilt on demand rather than a running process staying up.
Devin is the outlier: "session changes don't persist back to the snapshot" at all — a session is
always cold relative to the snapshot, by design, so there is no idle-reclaim question for Devin's
running layer to answer. Warmth is explicitly framed as degradable/best-effort rather than an SLA
wherever it is discussed: Claude Code notes background work (subagents, running shells) is *not*
restored after VM reclaim even though conversation history is; Codex's cache invalidates
automatically on any setup-script or variable change; Cursor's build "preserves disk state only" and
drops "running processes, shell exports, and in-memory caches."

**Snapshot/restore of *live* agent state — as opposed to a filesystem template — is the sharpest
differentiator, and most platforms do not offer it.** Three do, and they mean three different
things by it. E2B's pause/resume is the most complete: filesystem *and* memory, "all the files...
and all the running processes, loaded variables, data" restored bit-for-bit, at a cost proportional
to RAM (~4s/GiB to pause, ~1s to resume), because it is a microVM-level suspend, not an
application-level save. Replit's checkpoint is application-level and broader than the filesystem:
it captures "project files," "AI conversation context," "agent memory" (the model's own
understanding of the project), and optionally database contents — a product feature synthesized by
Replit's own Agent, not a generic infrastructure primitive, and its format is undocumented (opaque
to the user; only its *contents*, not its representation, are described). Claude Code sits in
between: what survives a reclaimed cloud-session VM is the *conversation transcript* (restored on
reopen) plus whatever the repository commit and the environment's filesystem cache captured — but
explicitly *not* in-flight subagents or shells, which the docs call out as lost. Modal and Daytona
offer filesystem-level snapshotting as a general sandbox primitive (an "Image" or "Snapshot" you can
spin a new sandbox from) but neither documents it as tied to a specific agent session's state; it is
infrastructure the agent runtime could use, not something the platform does automatically per turn.
No platform documents a portable, agent-runtime-owned "resume token" format that survives a change of
underlying compute vendor — every resume mechanism found is either whole-VM/whole-container
(E2B, Daytona, Modal) or whole-conversation-transcript-plus-fresh-clone (Claude Code, Devin, Codex),
owned entirely by the platform, not by an agent runtime distinct from the platform.

**`devcontainer.json` is real prior art but is one input among several, never universally the
source of truth.** Ona is the strongest adopter: "if you're already using the Dev Container
specification, Ona will automatically use your existing devcontainer.json," and Ona's own two-file
model (`devcontainer.json` for tools, `.ona/config.yaml` for tasks/services) treats the spec as the
base-environment layer with Ona-specific automation layered on top via `triggeredBy` hooks. Daytona
auto-detects a repo's `.devcontainer.json` as one of several snapshot sources alongside a bare image
or Dockerfile. GitHub itself documents a preconfigured dev container as *one* of six sandboxing
options for local/self-hosted Claude Code, not the cloud product's own mechanism — Claude Code's
actual cloud environments are configured through a platform-native dialog (network level, env vars,
setup script) with no devcontainer.json input at all. Copilot coding agent, Codex, Cursor, Devin,
and Jules document no devcontainer.json support whatsoever; each has its own declared-noun format
instead (`copilot-setup-steps.yml`, environment settings + setup script, `.cursor/environment.json`,
a YAML blueprint, a setup script triggered from a Configuration panel). The Dev Container
specification itself is deliberately silent on lifetime and warm/cold behavior — "it is up to the
implementing tool or service to determine when this event should happen" — which is exactly the gap
each platform's own declared noun fills differently.

**Git awareness is close to universal but the depth varies, and "runs git itself" is the norm, not
the exception, for the agent platforms (as opposed to the general-purpose sandboxes).** Copilot,
Codex, Cursor, Claude Code, and (implicitly) Devin all run git themselves: create a branch, commit,
push, open or update a PR, without the user typing a git command. Copilot pushes commits to a draft
PR as it works and requires human approval before CI runs on that PR's branch; Codex checks out the
selected branch/SHA into its container and shows "a diff of any files it changed" before the user
opens a PR; Claude Code computes per-file diffs "from raw git blob content" (bypassing repo-configured
diff drivers), attaches a `Claude-Session:` git trailer to every commit it creates, and routes all
GitHub API and git-credential traffic through a dedicated proxy that keeps the real token outside the
sandboxed VM entirely — the most detailed documented git-isolation model of any platform surveyed.
General-purpose sandbox platforms (E2B, Modal, Daytona) do not document git awareness as a platform
feature at all — git, if used, is just a program the agent runtime the user brought happens to run
inside the sandbox, observed by nobody but that runtime.

## Per-platform detail

### Ona (Gitpod)

Ona (the current, Gitpod-Flex-based product; "Gitpod Classic" is the predecessor product, still
documented separately) declares an **environment class** as a resource-profile noun distinct from
both the runner (infrastructure) and the running **environment** (an ephemeral, isolated workspace).
"A project continues to select environment classes, not individual instance types. An environment
can run on any instance type configured for its selected class," and compatible instance types
within a class must share vCPU count/generation, memory, CPU architecture/instruction set, and
GPU accelerator type/count — so the class is the project-facing declared noun, and instance type is
an infrastructure-level fallback resolved underneath it, with Ona attempting "ordered fallback
types" on availability
([Configure multiple instance types](https://ona.com/docs/ona/runners/multiple-instance-types.md)).
Selection is per-project, and environments are tracked with owner, associated project, runner,
status ("Running, stopped, or archived"), and creation timestamp
([Environment inventory](https://ona.com/docs/ona/organizations/environment-inventory.md)).
Configuration input is a declared two-file model: `.devcontainer/devcontainer.json` ("Your tools and
environment" — base image or Dockerfile, features, IDE config, setup commands) plus
`.ona/config.yaml` ("Your tasks and services" — one-time Tasks vs. long-running Services), with
`triggeredBy` hooks such as `postDevcontainerStart` letting user automation run relative to
devcontainer readiness
([Lab 2: Configuration](https://ona.com/docs/workshops/lab-2-configuration.md)). Ona explicitly
adopts the Dev Container spec as an input rather than reinventing it: "if you're already using the
Dev Container specification, Ona will automatically use your existing devcontainer.json." Lifetime is
warm-with-idle-expiry and user-configurable: the CLI has an `--inactivity-timeout` flag, and Core/
Enterprise accounts can set a default auto-stop timeout in preferences
([Changelog](https://ona.com/docs/changelog)). Gitpod Classic's older, more fully documented lifecycle
model — 30-minute default inactivity stop (configurable to 24h), 8h/36h absolute lifetime caps,
`/workspace`-only persistence across stop/restart, 14-day soft-delete then 21-day hard-delete unless
pinned or snapshotted — is presumably a design ancestor of Ona's model but is not confirmed to still
apply verbatim to the Flex-based product
([Workspace Lifecycle](https://ona.com/docs/classic/user/configure/workspaces/workspace-lifecycle.md)).
Ona's own agents page describes agents running in "isolated environments" with an Enterprise Runner
deployment option, but does not itself state whether an agent run gets a dedicated environment or
reuses one, nor its lifecycle beyond what the environment-inventory and class pages already establish
([Agents](https://ona.com/docs/ona/agents.md)).

### GitHub Copilot coding agent (and GitHub Actions runners as prior art)

Copilot's coding agent runs inside "an ephemeral development environment, powered by GitHub Actions,"
which makes the **GitHub Actions runner** — the well-known declared noun distinct from a running
job — the substrate. By default Copilot uses an Ubuntu Linux environment; it can be switched to
Windows, upgraded to a larger runner (more CPU/memory/disk) via the `runs-on` field of a special
`copilot-setup-steps` job, or routed to a self-hosted ARC runner. Organization owners can set and
lock a default runner type. The environment is customized entirely through a GitHub Actions workflow
file, `.github/workflows/copilot-setup-steps.yml` — no devcontainer.json input is documented — which
can preinstall dependencies, set env vars, enable Git LFS, and customize or disable the agent's
firewall
([Customize the agent environment](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/coding-agent/customize-the-agent-environment)).
This is per-repository selection (the workflow file lives in the repo, and must be on the default
branch to take effect); the docs do not describe per-task variation. Lifetime is ephemeral per
session with no warm reuse documented. Copilot runs git itself: it opens a draft PR immediately, and
"as the agent works, it pushes commits to a draft pull request... and updates the pull request's
description," restricted to branches it created itself; by default it pushes to that same PR branch,
though a comment can direct it to open a separate PR instead
([Using Copilot cloud agent on GitHub](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/cloud-agent/use-cloud-agent-on-github)).

As general prior art for the declared-noun question: a GitHub Actions **self-hosted runner** is a
registered entity distinct from any job it runs, carrying default labels (`self-hosted`, an OS label,
an architecture label) plus operator-assigned custom labels and group membership, with jobs matched
to runners by `runs-on` label/group
([Using labels with self-hosted runners](https://docs.github.com/en/actions/hosting-your-own-runners/managing-self-hosted-runners/using-labels-with-self-hosted-runners)).
Self-hosted runners default to persistent (reused across jobs) but can be registered `--ephemeral`,
in which case GitHub "will automatically de-register the runner after it has processed one job" —
i.e., warmth is a configuration choice on self-hosted infrastructure, not a platform default.
GitHub-*hosted* runners, by contrast, are cold by construction: "each GitHub-hosted runner is a new
virtual machine (VM) hosted by GitHub"
([About GitHub-hosted runners](https://docs.github.com/en/actions/using-github-hosted-runners/about-github-hosted-runners)).

### OpenAI Codex cloud

Codex's declared noun is the **environment**, configured in Codex settings and wrapping OpenAI's
open-source `codex-universal` base image (Ubuntu 24.04, polyglot, `ghcr.io/openai/codex-universal:latest`).
An environment holds the image, a setup script plus a separate maintenance script, environment
variables and secrets, a network access policy (agent internet access is off by default; can be set
to limited or unrestricted), and pinned runtime versions via `CODEX_ENV_*` variables for up to eight
languages. Selection is per-repository/workspace, configured in settings rather than per task, and
Business/Enterprise environments' caches are "shared across all users who have access to the
environment." Warmth is a filesystem cache, not a running process: "Codex caches container state for
up to 12 hours to speed up new chats and follow-ups," invalidated automatically on any setup/
maintenance-script or variable change, and manually resettable. No agent-state snapshot across turns
is documented — only the container cache. Setup scripts run "in a separate Bash session from the
agent," and shell-level effects like `export` do not persist into the agent's own session. No
devcontainer.json input is documented; `codex-universal` plus setup scripts is the only path. Codex
runs git itself: "Codex creates a container and checks out your repo at the selected branch or commit
SHA," and after the run shows "a diff of any files it changed. You can open a PR or ask follow-up
questions"
([Cloud environments](https://learn.chatgpt.com/docs/environments/cloud-environment)).

### Devin (Cognition)

Devin's declared noun is the **snapshot** — "a frozen, bootable image of the environment," explicitly
distinct from the **session** that boots from it: "Devin boots a fresh copy of the snapshot. Every
session starts from the same clean state," and critically, "session changes don't persist back to the
snapshot." Configuration scope is coarser than per-repository: "each organization has exactly one
active snapshot. Every session in that org boots from the same snapshot," built by concatenating
enterprise-level config, org-level config, then each configured repository's own `initialize`/
`maintenance` steps in sequence, cloning up to 10 repos concurrently. Multiple *golden snapshots* do
exist at the enterprise level as inheritable defaults per organization, but the docs do not describe
a single organization running more than one active snapshot at a time, nor per-task selection
([Environment Configuration](https://docs.devin.ai/onboard-devin/environment-yaml),
[Devin environment setup](https://docs.devin.ai/onboard-devin/environment)). What the snapshot holds:
"all cloned repositories and their contents, installed tools, compilers, and package managers,
package dependencies... environment variables and shell configuration, startup commands that run at
the beginning of each session"; secrets live separately in a blueprint editor's Secrets tab and are
"injected as environment variables during builds and sessions"
([Golden Snapshots](https://docs.devin.ai/product-guides/snapshots)). A build can also be produced
conversationally: "start a Devin session and ask it to configure the repo... you click Approve on the
suggestion cards." Lifetime: the snapshot itself is durable until rebuilt; the session is, by design,
always cold relative to it (no session-to-snapshot persistence), and the docs do not address whether
a *running* session itself is warm across turns, has an idle timeout, or can be paused/resumed with
live state intact — only that it boots from a fixed image. Git awareness (branch/diff/PR surfacing,
or whether Devin runs git itself) is not addressed in the environment/snapshot documentation read for
this research.

### Cursor cloud (background) agents

Cursor's declared noun is `.cursor/environment.json`, a per-repository (or repo-group) configuration
resolved by first match: the repo's own file, then a personal saved environment, then a team saved
environment. It holds a Dockerfile reference (`build.dockerfile`/`build.context`) or a saved
**snapshot** ("Cursor saves the machine's disk state with the environment version and exact commit
SHA for each repository"), an `install` command for ahead-of-time dependency setup, start commands,
and terminals. A **Build** prepares this in the background so agents start ready; layer caching
applies to Dockerfile changes. Warmth is explicitly disk-only: "Builds preserve disk state only.
Running processes, shell exports, and in-memory caches stop when Cursor snapshots the machine." No
devcontainer.json input is documented — `.cursor/environment.json` plus an optional Dockerfile is the
whole mechanism. Multi-repo environments exist for cross-repo agent tasks, letting an agent "open pull
requests in the repos it changes," implying Cursor's agent runs git/PR operations itself, though the
exact mechanics (branch naming, push semantics) are not detailed in the pages read
([Cloud Environment Setup](https://cursor.com/docs/cloud-agent/setup),
[Cloud Agents](https://cursor.com/docs/cloud-agent)).

### Google Jules

Jules runs each task in "a secure, short-lived virtual machine (VM)," with a distinct, optional
**environment snapshot** layered on top: for complex environments a user provides a setup script
under Configuration → Environment, and running "Run and Snapshot" both validates the script and
produces "a snapshot of your environment... used for future Jules tasks started from this repository,"
explicitly to amortize long setup times. For simple environments with no setup script, Jules instead
"studies your repository" and consults `AGENTS.md` or the README for setup hints on the fly. This is
per-repository configuration; the docs describe a single environment per repo, though they do not
explicitly rule out more than one. Warmth is snapshot-based reuse across tasks, matching the Codex/
Cursor pattern. Not documented: whether the snapshot captures full VM state or filesystem only (the
docs' own framing — "especially useful for complex environments with long setup times" — suggests
filesystem/dependency state, by inference rather than an explicit statement), devcontainer.json
support, and git branch/diff/PR surfacing beyond "clone your repository"
([Environment setup](https://jules.google/docs/environment/)).

### Claude Code on the web / cloud sessions (Anthropic)

Anthropic's declared noun is the **cloud environment**, distinct from a running **cloud session**:
"each cloud session runs in a cloud environment," configured with a name, a network access level
(None/Trusted/Full/Custom, the last taking an explicit domain allowlist), `.env`-format environment
variables, on Pro/Max plans out-of-band API credentials the agent proxy attaches without the session
ever seeing the key, and a Bash setup script that must exit zero and finish in roughly five minutes.
Selection: an account with only the onboarding-created **Default** environment (Trusted network,
no script) uses it for every session; with more than one, the web/mobile/Desktop surfaces use a UI
selector (falling back to an organization default an Owner sets), and the CLI uses a `/remote-env`
pick (falling back to the Anthropic-hosted environment, then the first non-bridge environment).
Environments can be personal or organization-shared (Team/Enterprise); a self-hosted environment
(`ccpool_...` ID) is a distinct environment *type* an operator stands up on its own infrastructure.
No per-task dynamic selection is documented; `--environment <id>` can override the pick per CLI
invocation but that is a caller choosing at dispatch, not the platform choosing based on task content
([Configure cloud environments](https://code.claude.com/docs/en/cloud-environments)). Warmth is a
filesystem cache: "after \[the setup script] completes, Anthropic snapshots the filesystem and reuses
that snapshot as the starting point for later sessions," rebuilt on setup-script/allowlist change or
roughly seven-day expiry; the cache "keeps what the setup script writes to disk and loses anything
that was only running" — a database the script started, or a `docker compose up` stack, does not
survive and must be started per session via prompt or a `SessionStart` hook. The *running session* is
separately reclaimed on inactivity ("cloud sessions stop after a period of inactivity and the
session's VM is reclaimed"); reopening "provision\[s] a fresh VM with your conversation history
restored," but explicitly *not* "background work that was still running when the VM was reclaimed,
such as subagents and shell commands" — a documented, named example of warmth being partial/lossy by
design rather than a full-state guarantee. devcontainer.json is not used as an input to cloud
environments at all; it appears only as one of six *local/self-hosted* sandboxing options (alongside
a sandboxed Bash tool, a sandbox-runtime process wrapper, a custom container, a VM, and Claude Code on
the web itself), where "a dev container runs Claude Code inside a Docker container that VS Code or a
compatible editor manages," defined by a `.devcontainer/` directory a user brings — one backend among
several, not the source cloud environments are derived from
([Choose a sandbox environment](https://code.claude.com/docs/en/sandbox-environments)). Git awareness
is the most thoroughly documented of any platform surveyed: cloud sessions include built-in GitHub
tools (read issues, list PRs, fetch diffs, post comments) authenticated through a dedicated GitHub
proxy that "keeps your real GitHub credentials outside the session's VM," restricts `git push` to the
session's own working branch, scopes API/release-asset requests to repositories attached to the
session, and serves only a pinned allowlist of GraphQL operations; diffs Claude shows are computed
"from raw git blob content" bypassing repo-configured diff drivers/`textconv`; every commit Claude
creates carries a `Claude-Session:` git trailer and every PR body a session link, both suppressible
via settings.

### Daytona

Daytona's declared noun is the **Snapshot** — "persistent, point-in-time captures of sandbox state,
including the filesystem, installed packages, dependencies, and settings," plus resource
specification (CPU/memory/storage/optional GPU) and OS/runtime baseline. A Snapshot is explicitly
selected at Sandbox-creation time ("Select a snapshot. Click Create"), and an organization can hold
several — built from a base image/registry pull, a custom Dockerfile, or a capture of an existing
running Sandbox's state. devcontainer.json is supported as one snapshot/sandbox source Daytona
auto-detects in a repo, alongside base images and Dockerfiles, not the sole or primary path
([Snapshots](https://www.daytona.io/docs/en/snapshots/)). As a general sandbox infrastructure product
rather than an agent product, Daytona's docs do not address agent-specific concepts: no documented
git-branch/PR surfacing, no documented distinction between a "run" and a "task," and no discussion of
setup-script vs. lifecycle-hook semantics beyond the Dev Container spec's own.

### E2B

E2B's declared noun is the **template**, built with `e2b template build` from a Dockerfile and
converted to a microVM snapshot; a running instance is a **Sandbox**. E2B's most distinctive
documented capability is true live-state pause/resume at the sandbox level: pausing preserves "the
sandbox's filesystem and memory state... all the files in the sandbox's filesystem and all the
running processes, loaded variables, data, etc.," and resuming restores that exact state, at a cost
of roughly 4 seconds per GiB of RAM to pause and roughly 1 second to resume; an `onTimeout` option can
downgrade this to filesystem-only (`keepMemory: false`) so idle sandboxes cold-boot instead. Paused
sandbox data is deleted after 30 days if never resumed
([Sandbox persistence](https://docs.e2b.dev/sandbox/persistence)). This pause/resume format is
entirely E2B's own — a generic infrastructure primitive exposed through the SDK (`sandbox.pause()`,
`sandbox.connect()`), not an agent-specific session directory or a format any agent runtime defines;
E2B's docs do not mention git, PRs, or diffs at all, consistent with E2B being a general code-execution
sandbox that an agent runtime (not documented by E2B itself) would run inside.

### Modal sandboxes

Modal's declared noun is the **Image**, which functions identically whether hand-authored or produced
by snapshotting a running Sandbox's filesystem: "containers are started up from a stored 'snapshot' of
their filesystem state called an image... Filesystem Snapshots are copies of the Sandbox's filesystem
at a given point in time, and these Snapshots are Images and can be used to create new Sandboxes."
Snapshots are filesystem-only (no documented memory/process capture, unlike E2B), garbage-collected
after a 30-day default TTL that is configurable or disable-able, and Sandboxes themselves are
described as long-lived and directly addressable, running under gVisor for isolation. Modal is a
general compute/sandbox platform; its docs do not address git, PR, or agent-turn concepts at all —
those would be the responsibility of whatever agent runtime a user runs inside a Sandbox.

### Replit Agent

Replit has no separately-declared environment noun distinct from the Repl itself; the closest
analogue is the Nix-based `replit.nix` dependency declaration, which defines packages rather than a
resource/image class. Replit Agent's distinguishing feature is the **checkpoint**: an automatic,
application-level save-state at development milestones, restoring "complete project state (all
project files return to their exact state...), AI conversation context..., project configuration...,
and development environment," with database contents included only when explicitly opted in. This is
the broadest scope of any "resume" mechanism surveyed — it includes the agent's own conversational
memory, not just filesystem/process state — but its underlying format is undocumented (only its
contents are described, as a product feature, not as an infrastructure primitive a third party could
target), and the docs do not state whether the underlying Repl compute itself is warm/persistent or
cold-started per session
([Checkpoints and rollbacks](https://docs.replit.com/core-concepts/agent/checkpoints-and-rollbacks)).

### The Dev Container specification

The spec formally separates a **development container** — "a definition (e.g. contained in a
`devcontainer.json` file) that deterministically creates containers" — from an **environment**, "a
logical instance of one or more development containers, along with any needed side-car containers,"
explicitly allowing "multiple runtime instances from one configuration." Lifetime phases (create,
stop, resume) are named, and lifecycle hooks (`initializeCommand` on the host;
`onCreateCommand`/`updateContentCommand`/`postCreateCommand` on first creation;
`postStartCommand` on every start; `postAttachCommand` on every editor attach) are specified in
detail, but the spec deliberately declines to mandate warm/cold behavior: "it is up to the
implementing tool or service to determine when \[a stop] should happen." The spec explicitly supports
multiple configurations per repository — "it is valid that these files may exist in more than one
location, so consider providing a mechanism for users to select one when appropriate" — at
`.devcontainer/devcontainer.json`, `.devcontainer.json`, or `.devcontainer/<folder>/devcontainer.json`
([Development Container Specification](https://containers.dev/implementors/spec/)). The spec is a
container-definition format, not an agent-execution model: it says nothing about git, runs, or agent
turns, which is exactly the gap every platform above fills with its own declared noun layered on top
of (Ona, Daytona) or instead of (everyone else) devcontainer.json.

## Gaps

Several questions the brief asked could not be answered from primary documentation for one or more
platforms, and are recorded here rather than guessed at:

- **Devin**: whether a running session (as opposed to the snapshot it boots from) is itself warm
  across turns, has any idle-timeout/reclaim policy, or supports pause/resume of live in-session
  state; whether Devin surfaces git branch/diff/PR state in its UI, and whether it runs git itself.
  The pages read (`onboard-devin/environment`, `onboard-devin/environment-yaml`,
  `product-guides/snapshots`) cover the build/snapshot layer thoroughly but do not reach session
  runtime behavior or git.
- **Jules**: whether an environment snapshot captures full VM state or filesystem/dependency state
  only (the docs' phrasing implies the latter but do not say so directly); whether a repository can
  have more than one environment; any devcontainer.json support; the mechanics of how Jules runs git
  and surfaces branch/diff/PR beyond "clone your repository."
- **Cursor**: exact push/branch/PR mechanics when an agent completes a task (the docs assert PR
  opening happens but do not detail branch naming or commit attribution); whether devcontainer.json
  is silently supported as a fallback when no `.cursor/environment.json` exists (not stated either
  way).
- **Ona**: whether the current, Flex-based product's stop/restart persistence and deletion-window
  behavior matches the more thoroughly documented Gitpod Classic model (30-minute default idle stop,
  `/workspace`-only persistence, 14-day soft-delete), or has been redefined; this research found only
  the newer `--inactivity-timeout` flag and an auto-stop preference, without the underlying default
  duration or persistence-on-stop semantics stated for the current product.
- **Replit**: the underlying compute model for a Repl (warm/persistent vs. cold-started), and the
  storage format of a checkpoint (only its logical contents are documented).
- **Daytona, E2B, Modal**: these are general-purpose sandbox/compute platforms, not agent products,
  so their docs are silent on git awareness, PR creation, and any run/task/turn concept by design —
  that silence is a property of the product category, not a documentation gap, but is noted since the
  brief asked the question of every platform.
- **GitHub Copilot coding agent**: whether `runs-on` (and therefore the runner/environment) can vary
  per individual task rather than being fixed per repository via the committed workflow file.
