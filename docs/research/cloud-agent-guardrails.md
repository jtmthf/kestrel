# How cloud coding agents enforce guardrails

This research pass covers nine hosted coding agents: Ona, Devin, OpenAI Codex (cloud), GitHub
Copilot cloud agent (formerly "coding agent"), Cursor Cloud Agents (formerly "background agents"),
Google Jules, Claude Code on the web, GitLab Duo Agent Platform flows and Factory remote
delegations. The last two are included because their first-party docs describe a guardrail feature
aimed at agents. It was gathered in September 2026 from primary sources only: official
documentation, changelogs and, for Codex, the open-source repository. It is prior art for the `0.4`
policy layer (`ROADMAP.md`, "0.4 — kestrel asks before it acts") and for the bound that
[ADR-0013](../adr/0013-an-event-supplies-data-never-authority.md) places on prompt injection. For
each product it asks three questions: where enforcement sits, what the policy is written in and
who reviews it, and what is recorded. Where a product's docs are silent, this file says so. Some
features a vendor documents only for its local client are cited here when they sit next to the
cloud product; each such case is labelled local. Terms follow `CONTEXT.md`: *policy*,
*disposition*, *approval*, *question*, *run*, *session* and *audit record*.

## Findings

**Every product that claims a hard boundary puts it outside the agent's process.** Those
boundaries are a network proxy, a proxy that holds credentials, the kernel, or the VCS host. None
of them is the agent loop. Codex cloud removes secrets before the agent phase starts and sends all
traffic through a proxy with a per-environment domain and HTTP-method allowlist
([cloud environments](https://learn.chatgpt.com/docs/environments/cloud-environment),
[internet access](https://learn.chatgpt.com/docs/cloud/internet-access)). Claude Code keeps git
credentials outside the VM. Its GitHub proxy accepts pushes only to the session's working branch
and serves only a pinned set of GraphQL operations
([cloud environments](https://code.claude.com/docs/en/cloud-environments#github-proxy)). Devin
enforces a security profile's network allowlist "at the machine level." Its git access level and
the removal of the `gh` token are enforced from Devin's cloud
([security profiles](https://docs.devin.ai/product-guides/security-profiles)). Ona's Veto is a
Linux Security Module that "the LLM cannot bypass or disable"
([Veto](https://ona.com/docs/ona/guardrails/veto.md)). Copilot and GitHub itself limit the agent
to one branch and to "simple push operations." They also subject it to branch protection, block
it from approving or merging its own work, and hold Actions workflows until a human approves them
([risks and mitigations](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/risks-and-mitigations)).
Cursor's boundaries are its network egress modes, the repository access it inherits from the
triggering user, and draft pull requests
([Cloud Agents security](https://cursor.com/docs/cloud-agent/security)). Jules documents only a
per-task VM "with internet access" and the GitHub App's repository selection
([FAQ](https://jules.google/docs/faq/)). Every product with agent-loop enforcement also documents
how that enforcement fails. Ona's command deny list is agent-only and does not stop the user's own
terminal ([command deny list](https://ona.com/docs/ona/command-deny-list.md)). Ona built Veto
because "path-based deny lists fall to renamed binaries"
([Veto](https://ona.com/docs/ona/guardrails/veto.md)). Copilot's firewall covers only processes
the agent starts through its Bash tool, and GitHub warns that "sophisticated attacks may bypass"
it ([firewall](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/cloud-agent/customize-the-agent-firewall)).
Claude Code's `permissions.ask` rules match command text, so `git -C <dir> push` evades
`Bash(git push *)`
([auto mode config](https://code.claude.com/docs/en/auto-mode-config#add-a-human-checkpoint)).
Copilot's hook runtime treats a timed-out `preToolUse` command hook as fail-open, "even for …
admin-deployed policy hooks"
([hooks reference](https://docs.github.com/en/copilot/reference/hooks-configuration)). Two
enforcement points fail open by design. Veto allows execution when the kernel cannot hash a binary
([Veto Exec](https://ona.com/docs/ona/organizations/policies/executable-deny-list.md)), and a
GitLab flow runs unsandboxed, with only a log warning, if its runner cannot create namespaces
([flow execution](https://docs.gitlab.com/user/duo_agent_platform/flows/execution/)).

**Tool-level hooks are the common policy surface inside the loop, and in the cloud they have no
human to ask.** Copilot, Cursor, Claude Code and Factory all run repository-defined hooks before a
tool executes. Copilot runs `.github/hooks/*.json`
([hooks](https://docs.github.com/en/copilot/concepts/agents/hooks)). Cursor runs
`.cursor/hooks.json` plus dashboard-distributed team and enterprise hooks
([hooks](https://cursor.com/docs/hooks)). Claude Code runs server-managed hooks, which require
approval ([server-managed settings](https://code.claude.com/docs/en/server-managed-settings)), and
Factory runs managed `hooks` ([enterprise controls](https://docs.factory.ai/enterprise/hierarchical-settings-and-org-control)).
Copilot's cloud agent treats a hook's `"ask"` as `"deny"` "because no user is available to
answer." Its `permissionRequest` hook "either does not fire or has no effect" because tool calls
are pre-approved ([hooks reference](https://docs.github.com/en/copilot/reference/hooks-configuration)).
Cursor Cloud Agents "auto-run terminal commands"
([security](https://cursor.com/docs/cloud-agent/security)) and run only command-based hooks
([hooks](https://cursor.com/docs/hooks)). Claude Code routines have "no permission-mode picker and
no approval prompts during a run," and every tool of an included connector is usable "including
writes, without asking" ([routines](https://code.claude.com/docs/en/routines)). Only three
products document an in-run approval in the hosted product that reaches a human. Devin asks before
reaching a blocked network destination, and that request can be answered from Slack
([security profiles](https://docs.devin.ai/product-guides/security-profiles),
[release notes, 24 July 2026](https://docs.devin.ai/release-notes/2026)); it also asks before
every public deploy ([deployments](https://docs.devin.ai/product-guides/deployment-capabilities)).
Claude Code cloud sessions offer Accept edits, Plan and Auto modes and can wait on an MCP
connector approval ([cloud sessions](https://code.claude.com/docs/en/claude-code-on-the-web)).
Jules has a plan approval ([planning](https://jules.google/docs/review-plan/)). The Codex cloud
pages read describe no in-run approval; review happens on the diff or the pull request
([Codex cloud](https://learn.chatgpt.com/docs/cloud)).

**Policy is written in four forms: allowlists, rule files, natural-language criteria for a model,
and VCS rules.** Allowlists are chosen in a UI or a schema. Codex environments have on/off,
presets and HTTP methods. Claude Code environments have four access levels plus custom domains.
Cursor has three egress modes at user, environment and team level, with an enterprise lock
([network](https://cursor.com/docs/cloud-agent/security-network#network-access)). Copilot has an
org and repo firewall where the org can forbid repository rules. Devin's security profiles cover
hostname and CIDR allowlists, an MCP allowlist, a git level and the `gh` token. Ona's command deny
list is a list of glob patterns. Structured rule files include Ona's `SecurityPolicy` YAML with
`EFFECT_AUDIT` and `EFFECT_BLOCK`
([Veto Exec](https://ona.com/docs/ona/organizations/policies/executable-deny-list.md)), Factory's
org-managed settings schema, which separates hard controls from session defaults
([enterprise controls](https://docs.factory.ai/enterprise/hierarchical-settings-and-org-control)),
and GitLab's `agent-config.yml` `network_policy`
([flow execution](https://docs.gitlab.com/user/duo_agent_platform/flows/execution/)). Three
products let prose drive a model's decision. The Codex reviewer policy is text that enterprises
replace with `guardian_policy_config`
([auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review)). Claude Code's
`autoMode.environment` tells the classifier which infrastructure is trusted
([auto mode config](https://code.claude.com/docs/en/auto-mode-config)). Cursor's PR approver reads
`APPROVAL_POLICY.md` files and a custom prompt
([PR Routing & Approval](https://cursor.com/docs/approval-agents)). **Few products document a
human review before a policy applies, and none documents a second person.** GitLab reads
`agent-config.yml` only from the default branch and recommends CODEOWNERS and approval rules for
that file ([security considerations](https://docs.gitlab.com/user/duo_agent_platform/flows/execution/security-considerations/)).
Cursor's approver will not use policy files that a pull request itself changes to relax review of
that same pull request ([PR Routing & Approval](https://cursor.com/docs/approval-agents)). Claude
Code shows each user a blocking security dialog for delivered hooks and credential-bearing sandbox
settings, and Claude Code exits if the user rejects them
([server-managed settings](https://code.claude.com/docs/en/server-managed-settings)). Ona and Devin
offer a staged rollout: Ona has audit-effect rules, and Devin has recommended profiles that can
later become mandatory. Neither requires the rollout. Several products apply policy only when a
session or environment next boots: Devin profiles, Ona's command deny list and Veto policies, and
Claude Code's launch-only settings. A running session can therefore keep a policy that has since
been replaced.

**Records are strong on attribution and weak on inputs.** Every product that documents an audit
surface records who acted and what changed. Few record what a decision was based on. The closest
matches are these:

- Devin's guardrail events record "the user message that triggered the guardrail," the rule that
  matched and the action taken
  ([AI Guardrails](https://docs.devin.ai/enterprise/features/ai-guardrails)).
- Codex auto-review items carry a rationale, a risk level and a user-authorization assessment
  (local desktop app, [auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review)).
- Copilot issue automations record "the reason behind" each change, including changes applied
  without review
  ([rationale and approvals](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/about-automation-rationale-and-approvals)).
- Copilot's streamed API usage records carry request and response bodies, in preview for EMU and
  data-residency enterprises
  ([agentic audit log events](https://docs.github.com/en/copilot/reference/agentic-audit-log-events)).

Some records leave the deciding rule or the inputs out:

- Claude Code's `tool_decision` event gives the verdict and a coarse `source`, but `config`
  "doesn't indicate which of these sources matched." Tool arguments appear only with
  `OTEL_LOG_TOOL_DETAILS=1`
  ([monitoring](https://code.claude.com/docs/en/monitoring-usage#tool-decision-event)).
- Ona's Veto entries omit `process.cmdline`
  ([audit logs](https://ona.com/docs/ona/audit-logs/overview.md)).
- Cursor's audit logs cover "authentication and admin events." Agent activity lives in the
  conversation state, which the Delete Agent API removes on demand and an enterprise retention
  policy can cap at 90 days ([security](https://cursor.com/docs/cloud-agent/security)).
- Jules deletes a task "permanently"
  ([managing tasks](https://jules.google/docs/tasks-repos/)) and documents no audit log.

Copilot is the only product that documents a durable link from the VCS artifact back to the
reasoning. Every agent commit message links to the session log, and audit events carry
`agent_session_id` for 180 days
([sessions](https://docs.github.com/en/copilot/how-tos/copilot-on-github/use-copilot-agents/manage-and-track-agents),
[audit events](https://docs.github.com/en/copilot/reference/agentic-audit-log-events)). Policy
changes themselves are audited with before-and-after values by Claude Code
([server-managed settings](https://code.claude.com/docs/en/server-managed-settings#audit-logging))
and by Factory ([audit log](https://docs.factory.ai/enterprise/audit-log)). Neither vendor
documents that a decision record names the policy revision that decided it.

**Contradicting evidence on #16: expiry, model judges and who may resolve.** Kestrel's rules are
that an approval's expiry resolves it as denied, that a model is the judge disposition and its
rejection goes to a human, and that only a human resolves an approval, with kestrel enforcing
that. The products contradict each rule in these ways:

- **Jules auto-approves on expiry.** "If you navigate away, Jules will eventually auto-approve the
  plan, which is set on a timer" ([planning](https://jules.google/docs/review-plan/)). Plans that
  need no human are reviewed by a model, the Planning Critic
  ([changelog](https://jules.google/docs/changelog/2026-01-26-1/)). The `approvePlan` endpoint
  accepts any API key holder, and the docs place no human requirement on the caller
  ([sessions API](https://jules.google/docs/api/reference/sessions/)).
- **Codex lets a model take the human's place on approval requests.** Auto-review routes approval
  requests "to a separate reviewer agent instead of stopping for a person." An organization can
  require the reviewer with `allowed_approvals_reviewers = ["auto_review"]`, which leaves no human
  in the seat. A denial goes back to the main agent, which "find[s] a materially safer path or
  stop[s] and ask[s] the user." That is the agent's choice, not an automatic escalation. A
  per-turn circuit breaker aborts the turn after 3 consecutive or 10 of 50 denials
  ([auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review),
  [managed configuration](https://learn.chatgpt.com/docs/enterprise/managed-configuration)). This
  is documented for local clients, not for Codex cloud.
- **Claude Code's classifier denies rather than escalates, including in cloud sessions**, where
  Auto is a selectable mode. A blocked action is denied and listed for manual retry. Human
  prompting resumes only after 3 consecutive or 20 total blocks
  ([permission modes](https://code.claude.com/docs/en/permission-modes#when-auto-mode-falls-back)).
- **Cursor's model resolves the VCS gate.** Its PR approver "can approve low-risk PRs," and the
  approval is posted as `cursor`
  ([PR Routing & Approval](https://cursor.com/docs/approval-agents),
  [automations](https://cursor.com/docs/cloud-agent/automations#identity)).
- **GitHub's own approvals for issue automations are not enforced.** They are "a workflow
  convenience, not a security control," with no server-side boundary. A model's self-rated
  confidence decides which changes wait for a human
  ([rationale and approvals](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/about-automation-rationale-and-approvals)).
- **Approval waits that end with the session.** A Claude Code session waiting on an MCP connector
  approval counts as idle "and it can expire during that wait." The docs do not say what the
  pending approval resolves to
  ([cloud sessions](https://code.claude.com/docs/en/claude-code-on-the-web#environment-expired)).
- **Separation of duties.** Copilot and GitLab go further than #16: the human who asked for the
  work cannot be the one who approves it. Copilot "prevents the user who asked … from approving
  it" ([risks and mitigations](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/risks-and-mitigations)).
  GitLab attributes the merge request to the triggering human so that segregation-of-duties rules
  apply ([composite identity](https://docs.gitlab.com/user/duo_agent_platform/composite_identity/)).
  Factory warns that the requester *can* approve a Factory-authored pull request unless a second
  review is required ([remote delegations](https://docs.factory.ai/remote-delegations)). Kestrel's
  rule excludes the run's agent but not the requesting human.

**On ADR-0013, the evidence is mostly stricter than kestrel, with one counterexample.** Claude Code
routines treat the saved prompt as the task and wrap webhook text in a `<routine-fire-payload>`
block labelled untrusted. The saved prompt must opt in before the routine acts on that text, and
a fired prompt "can't act as approval or consent"
([routines](https://code.claude.com/docs/en/routines)). Kestrel renders the brief *from* event
data, so it gives event text more standing than this. Copilot never presents comments from users
without write access to the agent. Its automations ignore events from such users by default, and
it strips hidden HTML-comment text before input reaches the agent
([risks and mitigations](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/risks-and-mitigations)).
ADR-0013 instead makes the actor filter opt-in and warns when it is absent. Devin screens incoming
messages and PR comments with a model and can block them
([AI Guardrails](https://docs.devin.ai/enterprise/features/ai-guardrails)). That is a
trigger-layer defence, where ADR-0013 says nothing at this layer solves injection. Devin's
`@devin` PR command requires write access and a linked account
([GitHub integration](https://docs.devin.ai/integrations/gh)), and Cursor does not fire PR
triggers from forks ([automations](https://cursor.com/docs/cloud-agent/automations)). The
counterexample is Ona's Linear integration: assigning an issue to Ona "on another person's behalf"
runs the session "as the issue's assignee"
([Linear](https://ona.com/docs/ona/integrations/configure-linear.md)). There, an event picks whose
credentials a run holds. Factory also picks identity from where a request arrives: a Slack DM runs
as the sender and a channel request runs as a service account. An admin configures that mapping,
not the event ([remote delegations](https://docs.factory.ai/remote-delegations)).

**Against Nora's three primitives from #3, only GitLab matches permission intersection exactly.
Two products scope writes by the request's origin, and nobody implements read taint.**

- **Permission intersection.** GitLab's composite identity gives a flow access equal to the
  triggering user's role or the service account's role, "whichever is more restrictive," and
  access to projects both can reach
  ([composite identity](https://docs.gitlab.com/user/duo_agent_platform/composite_identity/)).
  Cursor intersects for repository reach only: the org-level app install plus the triggering
  user's git access, "never widened"
  ([security](https://cursor.com/docs/cloud-agent/security)). Devin intersects policy layers,
  since a mandatory profile narrows every lower binding. For GitHub, however, Devin "uses the
  permissions granted at the organization level, not the permissions of the individual user"
  ([security profiles](https://docs.devin.ai/product-guides/security-profiles),
  [GitHub integration](https://docs.devin.ai/integrations/gh)). Ona agents act with the
  environment owner's SCM credentials
  ([source control](https://ona.com/docs/ona/source-control/overview.md)). Factory shared
  delegations act as a service account, not as the requester. Copilot checks write access at the
  trigger, then acts through its own narrowly scoped app. None of the last three documents a
  user ∩ agent intersection.
- **Contextual write scoping.** Copilot is closest. Triggered from a pull request, it can write
  only that pull request's branch; otherwise it can write only a fresh `copilot/` branch
  ([risks and mitigations](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/risks-and-mitigations)).
  Claude Code confines pushes to the session's working branch. Routines push to `claude/` branches
  and are refused a push to a protected branch, a branch with someone else's open pull request, or
  a branch with commits by another author ([routines](https://code.claude.com/docs/en/routines)).
  Devin binds a git access level per automation. Everywhere else, write scope is set per
  environment or per organization, not by the request.
- **Read taint.** No product documents a sticky rule where reading a source removes writes for the
  rest of a run. The nearest are per-action judgements that re-read the transcript. Claude Code's
  classifier blocks actions "driven by hostile content Claude read" and blocks sensitive-file
  content entering a commit, push or PR
  ([permission modes](https://code.claude.com/docs/en/permission-modes#what-the-classifier-blocks-by-default)).
  Codex auto-review checks for exfiltration against a compact transcript
  ([auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review)). Codex's secret removal
  before the agent phase is phase-based rather than triggered by a read
  ([cloud environments](https://learn.chatgpt.com/docs/environments/cloud-environment)). Cursor's
  approver refuses to trust policy files the pull request itself touched
  ([PR Routing & Approval](https://cursor.com/docs/approval-agents)).

## Per-platform detail

### Ona

**Where enforcement sits.** Ona enforces at two layers. The first is the agent loop: a command
deny list is checked before an agent-executed command runs. It blocks with an error telling the
agent not to retry, and it does not apply to the user's own terminal or to slash commands
([command deny list](https://ona.com/docs/ona/command-deny-list.md)). The second is the kernel:
Veto Exec, an LSM, audits or blocks executables by SHA-256 content identity for any process in the
environment. It resists renames, hard links, copies and symlinks, and it safelists Ona's own
binaries. If the kernel cannot hash a launch, "execution is allowed"
([Veto Exec](https://ona.com/docs/ona/organizations/policies/executable-deny-list.md)). The VCS
side is the user's own credentials: "agents inherit your permissions" and act with the environment
owner's SCM credentials ([source control](https://ona.com/docs/ona/source-control/overview.md)).
An admin can disable the agent's SCM API tools, leaving git only, and those tools cannot approve
PRs ([SCM tools](https://ona.com/docs/ona/agents/scm-tools.md)). Automations run as the user or as
a service account. They "cannot be configured to run as another human user"
([configure automations](https://ona.com/docs/ona/automations/configure-automations.md)). A Linear
assignment made on someone's behalf, however, runs as the assignee
([Linear](https://ona.com/docs/ona/integrations/configure-linear.md)). The pages read do not
describe an agent network egress allowlist.

**Policy form and review.** The command deny list is glob patterns in an admin-only settings page,
applied to new agent sessions. Veto Exec is `SecurityPolicy` YAML, managed in the dashboard or
through `ona organization security-policy`. Creation stores an inactive definition, validity is
checked on assignment, and the policy reaches environments only when they restart. Ona recommends,
but does not require, starting a rule at `EFFECT_AUDIT` before promoting it to `EFFECT_BLOCK`
([Veto Exec](https://ona.com/docs/ona/organizations/policies/executable-deny-list.md)). Plan mode
has the agent wait "for your approval before writing any code"
([changelog](https://ona.com/docs/changelog.md)). No other in-run human approval is documented.

**Records.** Enterprise audit logs record actor, subject, action, timestamp and kind. The kinds are
resource change, credential access and environment Veto. Routine status activity is omitted, and
`ListAuditLogs` does not return the `details` payload. Veto entries carry the filename and digest
but omit `process.cmdline`. Retention follows "your organization's data retention policy"
([audit logs](https://ona.com/docs/ona/audit-logs/overview.md)). The automation guardrails page
says "every execution is logged: commands, file changes, PRs created, errors"
([automation guardrails](https://ona.com/docs/ona/automations/guardrails.md)). The docs do not say
whether command-deny-list blocks reach the audit log, or whether conversations outlive environment
deletion.

### Devin

**Where enforcement sits.** Devin enforces at the sandbox and at its own cloud. A security profile's
network allowlist is enforced on Devin-managed VMs at the machine level and covers shell, browsing
and package installs. The MCP allowlist, the read-only Devin MCP setting, the git access level
(read-only or full) and the removal of the `gh` token are enforced from Devin's cloud. On Outposts,
Devin only publishes `spec.network_policy`, and the operator must enforce it. Child sessions
inherit the governing chain "so restrictions can't be escaped by delegating work"
([security profiles](https://docs.devin.ai/product-guides/security-profiles)). On the VCS side,
Devin uses organization-level GitHub App permissions, "not the permissions of the individual
user," and recommends branch protection. A linked user account changes only commit authorship
([GitHub integration](https://docs.devin.ai/integrations/gh)). OIDC tokens carry the session,
organization and requesting user for cloud-side attribution
([OIDC](https://docs.devin.ai/product-guides/oidc)). On the input side, AI Guardrails screen user
messages and PR comments with log, warn and block actions
([AI Guardrails](https://docs.devin.ai/enterprise/features/ai-guardrails)).

**Policy form and review.** Security profiles are set up in the UI at enterprise, organization,
automations-default, automation and session scope. A profile is *recommended*, meaning it can be
overridden, or *mandatory*, meaning lower selections are intersected with it and mid-session grants
are clamped. Managing profiles requires a dedicated permission. Changes take effect at the next
boot or wake ([security profiles](https://docs.devin.ai/product-guides/security-profiles)).
Guardrails are enterprise-wide presets with no per-organization setting
([AI Guardrails](https://docs.devin.ai/enterprise/features/ai-guardrails)). In-run approvals exist
for blocked network destinations, answerable in the session or from Slack
([release notes](https://docs.devin.ai/release-notes/2026)). They also exist for every public
deploy ([deployments](https://docs.devin.ai/product-guides/deployment-capabilities)) and for
proposed personal config changes ([plugins](https://docs.devin.ai/product-guides/plugins)). The
docs do not say who may answer a network request, or what happens if nobody does.

**Records.** The enterprise audit log API returns guardrail violations with the triggering message,
the rule and the action, plus configuration changes. It requires `ManageEnterpriseSettings`
([audit logs API](https://docs.devin.ai/api-reference/v3/audit-logs/enterprise-audit-logs),
[AI Guardrails](https://docs.devin.ai/enterprise/features/ai-guardrails)). Changelog entries add
MCP updates and secret link events to the audit log
([release notes](https://docs.devin.ai/release-notes/2026)). The docs do not say whether individual
tool calls or network-request approvals are audited. Customer data is retained "for the duration
of the relationship" ([security](https://docs.devin.ai/admin/security)).

### OpenAI Codex (cloud)

**Where enforcement sits.** Codex cloud enforces at the sandbox. Each task runs in an
OpenAI-managed container. The setup phase has network access, while the agent phase is offline by
default. Secrets are "removed before the agent phase starts." All outbound traffic passes through
an HTTP/HTTPS proxy
([cloud environments](https://learn.chatgpt.com/docs/environments/cloud-environment),
[approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security)). When access is
on, an environment can restrict it to a domain allowlist (None, Common dependencies or All) and to
`GET`, `HEAD` and `OPTIONS` ([internet access](https://learn.chatgpt.com/docs/cloud/internet-access)).
On the VCS side, the user reviews the diff and chooses to open a PR
([Codex cloud](https://learn.chatgpt.com/docs/cloud)). Codex "can push a fix back to the branch
when it has permission to do so" ([GitHub](https://learn.chatgpt.com/docs/third-party/github)).
Codex cloud entitlement does not grant repository permissions; the connected system governs those
([roles](https://learn.chatgpt.com/docs/enterprise/roles-and-workspace-permissions)). The pages
read do not say who may trigger `@codex`, which identity pushes, or whether a user ∩ app
intersection applies. The local clients add an agent-loop layer that the cloud pages do not
describe: approval policies, granular prompt categories, command rules and Auto-review
([approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security)). They also add
asynchronous safety monitoring that can pause a task.

**Policy form and review.** In the cloud, policy is per-environment UI settings. The managed
`requirements.toml` constrains local clients only. It "doesn't filter … Codex cloud traffic," and
cloud network access is configured "in its cloud environment settings"
([managed configuration](https://learn.chatgpt.com/docs/enterprise/managed-configuration)). The
Auto-review policy is natural-language text that `guardian_policy_config` replaces in managed
requirements, taking precedence over local `[auto_review].policy`
([auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review)). The docs link the default
policy at `codex-rs/core/src/guardian/policy.md`. That path returned 404 in September 2026, and
the reviewer implementation is in the
[guardian module](https://github.com/openai/codex/tree/main/codex-rs/core/src/guardian). No human
review step is documented for changes to environment settings.

**Records.** The cloud UI shows task logs and the "work log"
([internet access](https://learn.chatgpt.com/docs/cloud/internet-access)). The Compliance API is an
"append-only compliance log stream," but the guide defers event coverage and retention to the Admin
API reference, and it does not list Codex cloud tool events
([compliance API](https://learn.chatgpt.com/docs/enterprise/compliance-api)). OTel's
`codex.tool_decision` records approved or denied and whether the source was configuration or user
(local clients, [approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security)).
Auto-review items record status, rationale, risk level and user-authorization assessment in the
desktop app ([auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review)).

### GitHub Copilot cloud agent

**Where enforcement sits.** Copilot enforces mainly on the VCS side. Only users with write access
can trigger it, and comments from others "are never presented to the agent." It can push to one
branch only, through "simple push operations." It is subject to branch protection and required
checks. It cannot mark its PRs ready, approve them or merge them. The requesting user cannot
approve the resulting PR. An extra approval is required when a PR is unattributed. Actions
workflows wait for "Approve and run workflows"
([risks and mitigations](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/risks-and-mitigations)).
A ruleset incompatible with the agent blocks it entirely
([about](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/about-cloud-agent)). At the
sandbox, a firewall applies to Bash-tool processes, not to MCP servers or setup steps
([firewall](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/cloud-agent/customize-the-agent-firewall)).
In the agent loop, `preToolUse` hooks can allow, deny or modify a tool call; `ask` means deny,
command-hook errors fail closed, and timeouts fail open
([hooks reference](https://docs.github.com/en/copilot/reference/hooks-configuration)). MCP tools
listed in `tools` run "autonomously, and will not ask for your approval"
([MCP](https://docs.github.com/en/copilot/how-tos/copilot-on-github/customize-copilot/configure-mcp-servers)).
Built-in CodeQL, advisory-database and secret-scanning checks run before the PR completes
([risks and mitigations](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/risks-and-mitigations)).

**Policy form and review.** The firewall lives in "Internet access" settings at org and repo
level. Org allowlist entries cannot be deleted by a repository, and the org can forbid repository
rules ([firewall](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/cloud-agent/customize-the-agent-firewall)).
Hooks are JSON files in `.github/hooks/`. In the cloud, "the only hook configuration that exists
by default is `.github/hooks/*.json` inside the cloned repository"
([hooks reference](https://docs.github.com/en/copilot/reference/hooks-configuration)). The docs
do not say which branch that is read from, or whether a hook change needs review before it
applies. Automations select their tools explicitly and are enabled by users with write access
([automations](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/about-automations)).
The enterprise controls availability through four policy states
([enterprise management](https://docs.github.com/en/copilot/concepts/agents/enterprise-management)).

**Records.** Session logs show "internal reasoning and the tools it used." Every commit is signed,
authored by Copilot, co-authored by the requester, and links to the session log
([sessions](https://docs.github.com/en/copilot/how-tos/copilot-on-github/use-copilot-agents/manage-and-track-agents)).
Blocked firewall requests are written into the PR body or a comment together with the command
([firewall](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/cloud-agent/customize-the-agent-firewall)).
Enterprise audit events for `actor:Copilot` carry `actor_is_agent`, `agent_session_id` and the
initiating `user`, and are kept for 180 days. Streamed usage records include request and response
bodies (preview)
([audit events](https://docs.github.com/en/copilot/reference/agentic-audit-log-events)). Issue
automations record a rationale for each change
([rationale and approvals](https://docs.github.com/en/copilot/concepts/agents/cloud-agent/about-automation-rationale-and-approvals)).
The docs do not describe hook decisions reaching the audit log.

### Cursor Cloud Agents

**Where enforcement sits.** Cursor enforces at the sandbox and at the VCS host. Each agent gets a
Firecracker microVM in a separate AWS account. Repository access is the admin-installed app
intersected with the triggering user's git access, "never widened." Protected Git Scopes and a
repository blocklist narrow it further
([security](https://cursor.com/docs/cloud-agent/security)). Egress is controlled by three modes,
and the team setting can be locked
([network](https://cursor.com/docs/cloud-agent/security-network#network-access)). Runtime secrets
are redacted from the transcript, tool output and commits. Agents open draft PRs with signed
commits ([security](https://cursor.com/docs/cloud-agent/security)). In the agent loop, commands
auto-run, and command-based project, team and enterprise hooks run once the environment is
writable ([hooks](https://cursor.com/docs/hooks)). Automations do not fire PR triggers from forks.
Team-owned automations run as a shared service account, and approvals are posted as `cursor`
([automations](https://cursor.com/docs/cloud-agent/automations)).

**Policy form and review.** Network modes are dashboard settings at user, environment and team
scope. Precedence runs environment, then user, then team, unless an Enterprise lock applies
([network](https://cursor.com/docs/cloud-agent/security-network)). Hooks are `.cursor/hooks.json`
plus dashboard-distributed hooks ([hooks](https://cursor.com/docs/hooks)). The PR approver's
policy is prose in `APPROVAL_POLICY.md`, `ROUTING.md` and a custom prompt. It uses the
base-branch version of any policy file the PR changes, and requires a human review if that version
is unknown. On Enterprise, only team admins may edit the approver
([PR Routing & Approval](https://cursor.com/docs/approval-agents)). No human review before a
network or hook change applies is documented.

**Records.** Conversation state holds "prompts, model responses, tool calls, diff context." It is
kept indefinitely by default, removable through the Delete Agent API, and cappable at 90 days on
Enterprise. Audit logs cover "authentication and admin events" and can stream to a SIEM, webhook
or S3. The Cursor Cloud MCP exposes transcripts and run events
([security](https://cursor.com/docs/cloud-agent/security),
[network](https://cursor.com/docs/cloud-agent/security-network#data-retention)). The docs do not
say whether hook or egress denials are recorded outside the transcript. Automations' memories
persist across runs and are flagged as an injection risk
([automations](https://cursor.com/docs/cloud-agent/automations)).

### Google Jules

**Where enforcement sits.** Jules enforces at the sandbox and at the GitHub App. Each task runs in
a "fresh virtual machine … with internet access," and the user is told "you are responsible for
the code you run" ([FAQ](https://jules.google/docs/faq/)). Repository reach is the GitHub App's
selection ([managing tasks](https://jules.google/docs/tasks-repos/)). The user creates the branch
and Jules is the commit author ([running tasks](https://jules.google/docs/running-tasks/)); commit
authorship is configurable
([changelog](https://jules.google/docs/changelog/2026-02-19/)). MCP is limited to a hand-picked
server list ([changelog](https://jules.google/docs/changelog/2026-02-02/)). Tasks start from
adding a `jules` label to an issue, and the docs do not say which actors may trigger one
([running tasks](https://jules.google/docs/running-tasks/)). No network allowlist, command policy
or hook mechanism is documented.

**Policy form and review.** The only documented gate is plan approval, set per session in the API
with `requirePlanApproval`
([sessions API](https://jules.google/docs/api/reference/sessions/)). In the UI it auto-approves on
a timer ([planning](https://jules.google/docs/review-plan/)). A Critic agent reviews code, and a
Planning Critic reviews plans that need no human
([Critic](https://jules.google/docs/changelog/2025-08-083/),
[Planning Critic](https://jules.google/docs/changelog/2026-01-26-1/)). No organization-level
policy surface is documented.

**Records.** The Activities API lists session events, with originator and plan, message and
artifact payloads ([activities API](https://jules.google/docs/api/reference/activities/)). Deleted
tasks are "removed permanently" ([managing tasks](https://jules.google/docs/tasks-repos/)). No
audit log or retention policy is documented.

### Claude Code on the web

**Where enforcement sits.** Claude Code enforces at the sandbox and at credential proxies. Each
session runs in an isolated VM. A security proxy carries all outbound traffic, subject to an access
level. A GitHub proxy swaps a scoped in-VM credential for the user's token, accepts pushes only to
the working branch, and restricts GraphQL. On Pro and Max plans, an agent proxy attaches API keys
after requests leave the VM. MCP connector traffic, GitHub and credential hosts bypass the domain
allowlist ([cloud environments](https://code.claude.com/docs/en/cloud-environments),
[cloud sessions](https://code.claude.com/docs/en/claude-code-on-the-web#security-and-isolation)).
In the agent loop, permission rules, hooks and the Auto classifier apply. Cloud sessions offer
Accept edits, Plan and Auto; they ignore `dontAsk` and bypass modes from settings files
([permission modes](https://code.claude.com/docs/en/permission-modes)). Server-managed settings
apply inside cloud sessions; endpoint-managed settings do not
([server-managed settings](https://code.claude.com/docs/en/server-managed-settings)). Routines run
with no approval prompts and act "as you." Before pushing to a non-`claude/` branch, they check it
is not protected, not someone else's PR branch and free of other authors' commits
([routines](https://code.claude.com/docs/en/routines)). The classifier trusts only the working
directory and the remotes configured at session start; remotes added mid-session are not trusted
([permission modes](https://code.claude.com/docs/en/permission-modes#what-the-classifier-blocks-by-default)).

**Policy form and review.** Environments are UI settings: network level, allowlist, variables and
setup script. Owners manage shared environments, and "there's no organization-level allowlist"
([cloud environments](https://code.claude.com/docs/en/cloud-environments)). Permission rules and
hooks are JSON settings, either committed to the repository or server-managed. Delivered hooks and
certain sandbox and `env` settings show each user a blocking security dialog, and Claude Code exits
if the user rejects them
([server-managed settings](https://code.claude.com/docs/en/server-managed-settings)). The docs
describe that dialog for sessions that can display it, not specifically for cloud sessions.
`autoMode` takes prose environment entries and rule overrides
([auto mode config](https://code.claude.com/docs/en/auto-mode-config)). Boundaries a user states
in conversation block matching actions, but "are not stored as rules" and can be lost to
compaction
([permission modes](https://code.claude.com/docs/en/permission-modes#eliminate-prompts-with-auto-mode)).

**Records.** "All operations in cloud sessions are logged for compliance and audit purposes"; the
docs give no further detail ([security](https://code.claude.com/docs/en/security)). OTel
`tool_decision` events record decision, source and, when opted in, `tool_parameters`. Cloud
sessions carry the session's own identity attributes
([monitoring](https://code.claude.com/docs/en/monitoring-usage)). The docs do not say how a cloud
session's OTel export is configured, and none of the documented `source` values names the
classifier. Settings changes produce audit events with previous and new values
([server-managed settings](https://code.claude.com/docs/en/server-managed-settings#audit-logging)).
Denials made by the classifier are listed under **Recently denied**. A `PermissionDenied` hook
receives the denied input
([auto mode config](https://code.claude.com/docs/en/auto-mode-config#review-denials)). A reopened,
expired session restores conversation history but not background work
([cloud sessions](https://code.claude.com/docs/en/claude-code-on-the-web#environment-expired)).

### GitLab Duo Agent Platform (flows)

**Where enforcement sits.** GitLab enforces at identity, at the sandbox and at the VCS host. Flows
run in CI under a one-time composite identity. Its role is the lesser of the user's and the
service account's roles, and its reach is the projects both can access. The OAuth token is limited
to `ai_workflows` and `mcp` scopes, and CI job tokens are narrowed further
([composite identity](https://docs.gitlab.com/user/duo_agent_platform/composite_identity/)). By
default, network access reaches the GitLab instance only. Anthropic's sandbox runtime (SRT) wraps
agent commands but not `setup_script`, which runs with the triggering user's token in the
environment ([security considerations](https://docs.gitlab.com/user/duo_agent_platform/flows/execution/security-considerations/)).
Merge requests are attributed to the triggering human so that segregation-of-duties controls apply
([composite identity](https://docs.gitlab.com/user/duo_agent_platform/composite_identity/)).

**Policy form and review.** `.gitlab/duo/agent-config.yml` holds `network_policy`, `id_tokens`
and setup. It is read only from the default branch
([flow execution](https://docs.gitlab.com/user/duo_agent_platform/flows/execution/)). GitLab
recommends protected branches, CODEOWNERS and approval rules for the file, because a change to it
runs "in another user's runner environment"
([security considerations](https://docs.gitlab.com/user/duo_agent_platform/flows/execution/security-considerations/)).

**Records.** Composite identity embeds the human in the token "creating an audit trail," and agent
activity is attributed to the service account in audit logs and commits
([composite identity](https://docs.gitlab.com/user/duo_agent_platform/composite_identity/)). The
pages read do not describe audit event contents, retention or tool-decision records.

### Factory (remote delegations)

**Where enforcement sits.** Remote delegations are in Private Preview. Shared channel and issue
work runs as an admin-chosen service account with that account's git credentials and connectors.
Slack DMs run as the sender. Factory advises extra branch protection, because the requester can
approve a pull request authored by `factory-droid[bot]`
([remote delegations](https://docs.factory.ai/remote-delegations)). Org-managed hard controls cover
`maxAutonomyLevel`, command allowlists and denylists, the sandbox and its network, MCP policy,
managed hooks and retention. They apply "across laptops, CI, VMs, and airgapped environments"
([enterprise controls](https://docs.factory.ai/enterprise/hierarchical-settings-and-org-control)).
The docs do not say which of these the remote computers enforce.

**Policy form and review.** Policy is an org-managed settings schema. Hard controls take the first
value set and cannot be weakened below that level. Session defaults are capped by the hard controls
([enterprise controls](https://docs.factory.ai/enterprise/hierarchical-settings-and-org-control)).
No review step before a change applies is documented.

**Records.** The org audit log records actor, source, target and an ID-only payload.
Managed-settings updates carry revision tracking for before-and-after diffs, visible to Enterprise
owners only ([audit log](https://docs.factory.ai/enterprise/audit-log)). Per-action decision
records for delegated sessions are not documented in the pages read.

## Gaps

Questions the brief asked that primary documentation did not answer are recorded here rather than
guessed at:

- **Codex cloud**: who may trigger `@codex` on GitHub, which identity pushes, whether Compliance API
  events cover cloud tool activity, and where the reviewer policy now lives, since the documented
  link is dead.
- **Copilot**: which branch cloud hooks are read from, and whether hook decisions reach the audit
  log.
- **Devin**: who may approve a network access request, what an unanswered request resolves to, and
  whether tool-level actions are audited.
- **Ona**: any agent network egress control, whether command-deny-list blocks are audited, and how
  long conversations are retained.
- **Cursor**: whether hook or egress denials are recorded anywhere that survives transcript
  deletion.
- **Jules**: any network, command or organization policy; which actors may trigger by label; any
  audit log.
- **Claude Code on the web**: what "all operations … are logged" contains and who can read it;
  what a pending MCP approval resolves to when the session expires; whether the managed-settings
  security dialog is shown in cloud sessions.
- **GitLab and Factory**: audit contents for agent actions. For Factory, which managed controls the
  remote computers enforce.
