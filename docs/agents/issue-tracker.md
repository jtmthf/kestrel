# Issue tracker: GitHub

Issues and specs for this repo live as GitHub issues in **jtmthf/kestrel**. Use the `gh` CLI for all operations.

This repo has no git remote configured yet. Until one pointing at `jtmthf/kestrel` is added, pass `--repo jtmthf/kestrel` explicitly on every `gh` call below. Once the remote exists, `gh` infers the repo automatically and `--repo` can be dropped.

## Conventions

- **Create an issue**: `gh issue create --repo jtmthf/kestrel --title "..." --body "..."`. Use a heredoc for multi-line bodies.
- **Read an issue**: `gh issue view <number> --repo jtmthf/kestrel --comments`, filtering comments by `jq` and also fetching labels.
- **List issues**: `gh issue list --repo jtmthf/kestrel --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters.
- **Comment on an issue**: `gh issue comment <number> --repo jtmthf/kestrel --body "..."`
- **Apply / remove labels**: `gh issue edit <number> --repo jtmthf/kestrel --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <number> --repo jtmthf/kestrel --comment "..."`

## Difficulty

A ticket an agent will pick up carries exactly one `difficulty:` label: a spec, a `to-tickets` ticket, or a wayfinder AFK ticket. External PRs and HITL-only tickets do not. The five rungs and what each means are in `triage-labels.md`; this section is how they travel and what dispatch does with them.

- **Assign at creation.** Publishing a ticket is where the rung is chosen: propose one and let the user confirm it. Apply it with `gh issue edit <number> --repo jtmthf/kestrel --add-label difficulty:<rung>`. A ticket that arrives carrying none is treated as `difficulty:moderate`, and dispatch names that assumption rather than letting it pass silently.
- **Revise during triage.** When the work turns out harder or easier than it read, move it: `--remove-label` the old rung as you `--add-label` the new, so the ticket never carries two.
- **Query by rung.** `gh issue list --repo jtmthf/kestrel --label difficulty:<rung>`.

### Dispatching by difficulty

The rung chooses a **tier of model**, not a model. Effort is a second, independent dial.

| Rung | Model tier |
| --- | --- |
| `trivial` | the smallest, fastest model |
| `easy` | a fast, cheap model |
| `moderate` | a balanced model |
| `hard` | a strong reasoning model |
| `extreme` | the strongest reasoning model available |

Effort is a per-model variant, named differently in every harness, and no rung derives it. Set it to the work in front of you: a well-specified but reasoning-heavy ticket can want a cheap model at its highest variant, and a broad, shallow one the strongest model at its lowest. You choose the model and the effort; no harness picks either from the ticket.

Model names do not live in this document because the catalogs move. Read the live one from the harness you dispatch to:

- **OpenCode** — `opencode models` lists `provider/model`; a variant is `provider/model#variant`.
- **Codex** — `codex debug models` prints the catalog as JSON, each model with its reasoning levels, its default, and its capability prose.
- **Claude Code** — no enumeration surface; the `/model` picker is the list, so choose the model before dispatch.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

When set to `yes`, PRs run through the same labels and states as issues, using the `gh pr` equivalents:

- **Read a PR**: `gh pr view <number> --comments` and `gh pr diff <number>` for the diff.
- **List external PRs for triage**: `gh pr list --state open --json number,title,body,labels,author,authorAssociation,comments` then keep only `authorAssociation` of `CONTRIBUTOR`, `FIRST_TIME_CONTRIBUTOR`, or `NONE` (drop `OWNER`/`MEMBER`/`COLLABORATOR`).
- **Comment / label / close**: `gh pr comment`, `gh pr edit --add-label`/`--remove-label`, `gh pr close`.

GitHub shares one number space across issues and PRs, so a bare `#42` may be either: resolve with `gh pr view 42` and fall back to `gh issue view 42`.

## When a skill says "publish to the issue tracker"

Create a GitHub issue.

## When a skill says "fetch the relevant ticket"

Run `gh issue view <number> --comments`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body. `gh issue create --label wayfinder:map`.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (`gh api` on the sub-issues endpoint). Where sub-issues aren't enabled, add the child to a task list in the map body and put `Part of #<map>` at the top of the child body. Labels: `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving dev.
- **Blocking**: GitHub's **native issue dependencies**, the canonical, UI-visible representation. Add an edge with `gh api --method POST repos/jtmthf/kestrel/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is the blocker's numeric **database id** (`gh api repos/jtmthf/kestrel/issues/<n> --jq .id`, _not_ the `#number` or `node_id`). GitHub reports `issue_dependencies_summary.blocked_by` (open blockers only, the live gate). Where dependencies aren't available, fall back to a `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is unblocked when every blocker is closed.
- **Frontier query**: list the map's open children (`gh issue list --state open`, scoped to the map's sub-issues / task list), drop any with an open blocker (`issue_dependencies_summary.blocked_by > 0`, or an open issue in the `Blocked by` line) or an assignee; first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me`, the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then `gh issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.
