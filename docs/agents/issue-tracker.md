# Issue tracker: GitHub

Issues and specs for this repo live as GitHub issues in **openkestrel/kestrel**. Use the `gh` CLI for all operations.

## Conventions

- **Create an issue**: `gh issue create --repo openkestrel/kestrel --title "..." --body "..."`. Use a heredoc for multi-line bodies.
- **Read an issue**: `gh issue view <number> --repo openkestrel/kestrel --comments`, filtering comments by `jq` and also fetching labels.
- **List issues**: `gh issue list --repo openkestrel/kestrel --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters.
- **Comment on an issue**: `gh issue comment <number> --repo openkestrel/kestrel --body "..."`
- **Apply / remove labels**: `gh issue edit <number> --repo openkestrel/kestrel --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <number> --repo openkestrel/kestrel --comment "..."`

## Projects

The backlog has a GitHub Project, [kestrel](https://github.com/users/jtmthf/projects/3), linked to
this repository. It is a **projection**: it carries no field, priority or state of its own. Labels,
milestones and native dependencies are the only state, and every view is a filter over them; the
built-in `Status` field stays unused. Nothing writes to the Project, and no skill needs to — the
workflow stays `gh issue edit` and `gh api` against the issues themselves.

Four views, defined by filter:

| View               | Layout | Filter                                                            | Ordered / grouped by   |
| ------------------ | ------ | ----------------------------------------------------------------- | ---------------------- |
| Triage             | table  | `is:open is:issue -label:ready-for-agent,ready-for-human,wontfix`  | default                |
| Ready              | table  | `is:open is:issue label:ready-for-agent`                           | milestone, ascending   |
| Dispatch frontier  | table  | `is:open is:issue label:ready-for-agent no:assignee`               | milestone, ascending   |
| By rung            | table  | `is:open is:issue`                                                 | grouped by Milestone   |

Triage uses negation rather than `label:needs-triage` so it also catches issues carrying no label,
which `/triage` treats as never triaged. The Dispatch frontier view is a *proxy*: Projects documents
no `is:blocked` qualifier, so it cannot exclude blocked items and the blocked indicator is read off
the item. The authoritative frontier query is the one under "Wayfinding operations" below.

### Milestones are rungs

Every milestone is a rung of the ladder in `ROADMAP.md`. `0.2`–`0.7` and `v1` exist; `0.1` is not
created because it is closed.

- **An issue carries a rung once it leaves triage.** `ready-for-agent` and `ready-for-human` issues
  name a milestone; `needs-triage` and `needs-info` issues may leave it empty, and show up in the By
  rung view's unplaced group.
- **Assign at creation**, to the rung the work actually lands in rather than the current one — a
  `0.6` integration ticket takes `0.6` while the project sits at `0.2`.
- **Revise during triage** when work belongs to another rung:
  `gh issue edit <n> --repo openkestrel/kestrel --milestone "<rung>"`.
- Milestones carry no due dates. The ladder is deliberately dateless.

## Difficulty

A ticket an agent will pick up carries exactly one `difficulty:` label: a spec, a `to-tickets` ticket, or a wayfinder AFK ticket. External PRs and HITL-only tickets do not. The five rungs and what each means are in `triage-labels.md`; this section is how they travel and what dispatch does with them.

- **Assign at creation.** Publishing a ticket is where the rung is chosen: propose one and let the user confirm it. Apply it with `gh issue edit <number> --repo openkestrel/kestrel --add-label difficulty:<rung>`. A ticket that arrives carrying none is treated as `difficulty:moderate`, and dispatch names that assumption rather than letting it pass silently.
- **Revise during triage.** When the work turns out harder or easier than it read, move it: `--remove-label` the old rung as you `--add-label` the new, so the ticket never carries two.
- **Query by rung.** `gh issue list --repo openkestrel/kestrel --label difficulty:<rung>`.

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
- **Blocking**: GitHub's **native issue dependencies**, the canonical, UI-visible representation. Add an edge with `gh api --method POST repos/openkestrel/kestrel/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is the blocker's numeric **database id** (`gh api repos/openkestrel/kestrel/issues/<n> --jq .id`, _not_ the `#number` or `node_id`). GitHub reports `issue_dependencies_summary.blocked_by` (open blockers only, the live gate). Where dependencies aren't available, fall back to a `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is unblocked when every blocker is closed.
- **Frontier query**: list the map's open children (`gh issue list --state open`, scoped to the map's sub-issues / task list), drop any with an open blocker (`issue_dependencies_summary.blocked_by > 0`, or an open issue in the `Blocked by` line) or an assignee; first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me`, the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then `gh issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.
