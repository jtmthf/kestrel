# CI gates the merge, and a queue lands the batch

kestrel's own changes arrive several at a time: a person keeps three or four worktrees in flight, each
opens a pull request, and integrating them is a serial chore — merge one, `main` moves, rebase the
next, wait for its checks again. The waiting was not the expensive part; the recurring re-validation
was, and a suite that is red on roughly two pushes in five for reasons unrelated to the change made a
red run stop meaning anything. So CI stops being a report and becomes **the gate**: one required
workflow, on `pull_request` and on `merge_group`, whose green permits a merge, and a **merge queue**
that validates a batch of pull requests against the latest `main` without a person serialising them.

Three platform constraints shape the design, and each is recorded because a reader will otherwise
"fix" it back:

- **Change detection is a job, never a path filter.** `merge_group` accepts no `paths:` filter, so the
  required workflow always starts and its first job decides which heavy jobs run. A **job** skipped by
  a condition reports success; a **workflow** skipped by filtering leaves its required check pending
  forever. That is also why the required status is one always-running aggregate job rather than the
  individual ones.
- **The merge queue re-runs the required set for every entry.** An entry is a synthetic merge of the
  pull request with the latest `main`, and validating that combination is precisely what a serial
  human merge gets wrong. Re-runs are made cheap rather than skipped.
- **Cache writes happen only on trusted triggers.** `pull_request` and `merge_group` runs receive
  read-only cache tokens, so the trusted `push` to `main` is what keeps caches warm for everyone else.
  Letting the queue write its own caches would let pull-request code seed what `main` later restores,
  which a public repository will not accept.

## Considered options

- **Reuse a verdict keyed by the tree's content hash**, reporting green without running when that tree
  has passed before. Rejected: the tests are not hermetic, so it would record a flaky pass permanently
  and turn a flaky failure into a blocked tree — a flake made durable.
- **Gate on a fast subset**, leaving the heavy jobs advisory. Rejected: it reintroduces the failure
  this decision exists to remove, `main` breaking after a merge and being discovered later.
- **Path filtering on the required workflow.** Impossible: `merge_group` supports none.
- **Require a review.** Rejected: a pull request here is authored with the maintainer's own token, so
  the author and the only reviewer are the same person and every pull request would deadlock.
  Requiring status checks alone closes the same gap.
- **Let the queue write caches** (`cache-mode: write`). Rejected: on a public repository that lets
  pull-request code seed what `main` trusts.
- **Pay for runners, or run our own.** Rejected: the repository is public, where standard runners and
  native arm64 runners cost nothing.

## Consequences

- The flakes become a prerequisite rather than a nuisance. A gate over a flaky suite blocks good
  changes instead of protecting `main`, so they are fixed before anything is required.
- A `push` to `main` does work a pull request no longer does: it warms the caches, and publishes when
  the image inputs changed.
- The environment image and the control-plane image are built once per change and consumed by tag, so
  the compose suite stops rebuilding what its sibling jobs already built.
- The arm64 image check runs on an arm64 machine instead of emulating one, which removes most of its
  runtime.
- `publish` keeps its from-a-clean-checkout build and its reproducibility, and runs when the image's
  inputs change and on tags, so `latest` tracks the image inputs rather than every push.
- Change detection has a default: a path matching no job's inputs runs everything. A required check
  that silently does not run is worse than a slow one.
