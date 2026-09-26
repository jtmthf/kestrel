# How ADEs lay out a live agent session

Research pass for [#316](https://github.com/openkestrel/kestrel/issues/316), gathered 26 September 2026
from primary sources: the four open-source apps' repositories read at a pinned commit, their own docs
and changelogs, and Conductor's docs, changelog and changelog screenshots (Conductor is closed source).
Where a source was silent, this note says so instead of inferring. It uses kestrel's vocabulary
(`CONTEXT.md`): each app's thread/chat/session is called a *session* here only where the mapping is
exact; otherwise the app's own word is kept and glossed.

| App | Version read | Source |
| --- | --- | --- |
| T3 Code | `95030dc67` (26 Sep 2026); latest release v0.0.42 (16 Sep) | [pingdotgg/t3code](https://github.com/pingdotgg/t3code) |
| opencode desktop / web app | `b65de4d69` (26 Sep); `@opencode-ai/desktop` 1.18.32 | [anomalyco/opencode](https://github.com/anomalyco/opencode) (the `sst/opencode` repo now redirects here) |
| Orca | `da6d483ab` (26 Sep); latest release v1.4.212 (25 Sep) | [stablyai/orca](https://github.com/stablyai/orca), site [onOrca.dev](https://onOrca.dev) |
| Superset | `2370f60d4` (26 Sep); desktop v1.30.2 (22 Sep) | [superset-sh/superset](https://github.com/superset-sh/superset) |
| Conductor | changelog through 0.87.5 (25 Sep) | [conductor.build/docs](https://www.conductor.build/docs), [changelog](https://www.conductor.build/changelog.md) |

Two of the five are not transcript-first. **Orca** and **Superset** run agent CLIs in terminals and learn
status from hooks and OSC titles; a structured transcript is layered on top — Orca's "Chat UI" is behind
**Settings → Experimental** ([native-chat.mdx](https://github.com/stablyai/orca/blob/da6d483ab/docs/site/content/docs/agents/native-chat.mdx)),
and Superset's chat pane is behind a `CHAT_V3` feature flag and otherwise shows "Chat is being reworked"
([usePaneRegistry.tsx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/desktop/src/renderer/routes/_authenticated/_dashboard/v2-workspace/%24workspaceId/hooks/usePaneRegistry/usePaneRegistry.tsx#L707),
[ChatUnderConstruction.tsx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/desktop/src/renderer/components/ChatUnderConstruction/ChatUnderConstruction.tsx)).
Their transcript findings below describe code that most users do not see by default.

## Findings

**Nobody puts the queue in the sidebar.** Every app that queues follow-ups shows them *at the composer
end of the transcript*, never in the session list. T3 Code renders each queued message as a dashed user
bubble after the live rows, with a "Queued" clock chip whose tooltip says *why* it waits ("Sends after
the next tool call or when the turn ends", "Sends after the messages above it", "Waits for Send now"),
plus Send-now and Cancel-to-composer buttons
([MessagesTimeline.tsx L1777–1878](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/chat/MessagesTimeline.tsx#L1777-L1878),
[composer.md](https://github.com/pingdotgg/t3code/blob/95030dc67/docs/user/composer.md#send-while-the-agent-is-working)).
opencode docks a collapsible tray on top of the composer — "3 queued messages", first message as a
one-line preview when collapsed, Send now and Edit per item
([session-followup-dock.tsx](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/app/src/pages/session/composer/session-followup-dock.tsx)).
Conductor's redesigned queue (0.54.0, 18 May 2026) is the same shape: a "Queue paused · 3 queued" panel
stacked above the composer with Resume, each item deletable, sendable or editable, and the queue pausing
while the user edits ([0.54.0](https://www.conductor.build/changelog/0.54.0-new-queue-diff-diffs.md);
the release GIF shows the panel sitting directly on the composer). Orca echoes a send as an optimistic
"queued" bubble at the end of the transcript until the real user turn lands
([native-chat-pending.ts](https://github.com/stablyai/orca/blob/da6d483ab/src/renderer/src/components/native-chat/native-chat-pending.ts)).
Superset's chat composer offers only Stop while a turn runs; no queue UI was found
([SessionView.tsx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/desktop/src/renderer/routes/_authenticated/_dashboard/v2-workspace/%24workspaceId/hooks/usePaneRegistry/components/ChatV3Pane/components/SessionView/SessionView.tsx)).

**Queue vs. steer is a user setting in three of five.** T3 Code (**Follow-up behavior**: Queue default /
Steer, with `mod+Enter` doing the opposite for one message —
[settings.ts L453](https://github.com/pingdotgg/t3code/blob/95030dc67/packages/contracts/src/settings.ts#L453),
[keybindings.md](https://github.com/pingdotgg/t3code/blob/95030dc67/docs/user/keybindings.md#composer-controls)),
Conductor (same label, "Queue messages to send after the agent finishes, or steer the agent mid-turn",
0.50.0 — [changelog](https://www.conductor.build/changelog/0.50.0-steering.md)), and opencode (same
label and options; at this commit the default is Steer and the settings store rewrites a stored
`queue` to `steer` — [settings.tsx L187, L353–356](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/app/src/context/settings.tsx#L353-L356)).
Conductor's API is explicit that a message sent while working is steered into the running turn, and
that a session reports `idle` until a queued brief is delivered
([llms-full.txt, API guide](https://www.conductor.build/llms-full.txt)). T3's Stop returns every queued
message to the composer; Conductor's cancel "drops queued messages".

**Collapsed tool activity reads as one live line naming the current tool.** T3 Code groups a turn's
consecutive tool calls and thoughts into one activity row. While active, its label is the latest tool
("Running git", "Failed pnpm", a file path, or a tool's display name) or "Thinking" with a shimmer; once
settled it summarizes the group ("Thought (×3)" or a tool-group summary), and a failed tool tints the
icon and announces "tool call failed"
([MessagesTimeline.tsx L2608–2700](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/chat/MessagesTimeline.tsx#L2608-L2700),
[MessagesTimeline.logic.ts L72–97](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/chat/MessagesTimeline.logic.ts#L72-L97)).
Orca does the same per "tool run": one sentence for the run (or the command itself when the run is one
call — "the reader recognizes `git push` faster than 'Ran 1 command'"), the latest call's label beside
it while live, and a quiet "N failed" mark when settled; the run stays live for the whole turn rather
than flipping per call
([NativeChatToolRun.tsx](https://github.com/stablyai/orca/blob/da6d483ab/src/renderer/src/components/native-chat/NativeChatToolRun.tsx),
[native-chat-tool-activity.ts](https://github.com/stablyai/orca/blob/da6d483ab/src/shared/native-chat-tool-activity.ts)).
opencode folds read/glob/grep/list into one "Exploring" → "Explored" group with counts ("3 reads, 2
searches"), and its status titles animate from an active verb to a done verb sharing a prefix
([message-part.tsx L607, L1043–1082](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/session-ui/src/components/message-part.tsx#L1043-L1082),
[tool-status-title.tsx](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/session-ui/src/components/tool-status-title.tsx),
[ui i18n L85–110](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/ui/src/i18n/en.ts#L85-L110)).
Superset groups consecutive tool calls under "N tool calls", open while the turn runs and collapsed once
it settles, unless a call awaits approval
([TurnGroupSection.tsx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/desktop/src/renderer/routes/_authenticated/_dashboard/v2-workspace/%24workspaceId/hooks/usePaneRegistry/components/ChatV3Pane/components/Transcript/components/TurnGroupSection/TurnGroupSection.tsx)).

**The current thought gets a one-line summary too.** T3 Code's collapsed reasoning block shows the
thought's text flattened to one truncated line under a brain icon, labelled "Thinking" while it streams
and "Thought" after ([MessagesTimeline.tsx L2743–2800](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/chat/MessagesTimeline.tsx#L2743-L2800)).
opencode, with reasoning summaries hidden (the default), shows "Thinking" plus the *latest Markdown
heading* found in the reasoning text, revealed with an animation
([session-turn.tsx L371–431](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/session-ui/src/components/session-turn.tsx#L371-L431));
its strings also carry "Thinking - {{topic}}". Superset's reasoning element auto-opens while streaming,
auto-closes one second after, and then reads "Thought for N seconds"
([reasoning.tsx](https://github.com/superset-sh/superset/blob/2370f60d4/packages/ui/src/components/ai-elements/reasoning.tsx)).
Conductor toggles Claude's thinking visibility with `⌘⇧.` (0.15.1,
[changelog](https://www.conductor.build/changelog/0.15.1-claude-s-thinking.md)); how its collapsed
thinking reads was not established.

**A finished turn folds to "Worked for 4m".** T3 Code hides a settled turn's work behind a fold row
"Worked for {duration}" (or "You stopped after {duration}" when interrupted, which also leaves the turn
expanded) and keeps the final assistant message visible
([MessagesTimeline.logic.ts L780–830](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/chat/MessagesTimeline.logic.ts#L780-L830),
[MessagesTimeline.tsx L716–735](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/chat/MessagesTimeline.tsx#L716-L735)).
While working, a header reads "Working for {live timer}". Orca uses the same three states — "Thinking",
"Working for N", "Worked for N" with a caret to expand
([NativeChatWorkingStatus.tsx](https://github.com/stablyai/orca/blob/da6d483ab/src/renderer/src/components/native-chat/NativeChatWorkingStatus.tsx)).
opencode records a per-turn duration and, after the turn, an "N Changed files" diff group with Show all
([session-turn.tsx L337–480](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/session-ui/src/components/session-turn.tsx#L337-L480)).
This is the main device all three use to keep a long thrashing turn readable; the other is virtualized,
content-visibility rows (Superset's `[content-visibility:auto]`, T3's LegendList, Conductor 0.9.1
"we virtualized!").

**Global expand settings are rare and narrow.** Only opencode ships one, and it is per tool family:
**Expand shell tool parts** and **Expand edit tool parts** (both off), plus **Show reasoning summaries**
(off) ([settings.tsx L32–34, L193–195](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/app/src/context/settings.tsx#L193-L195),
[app i18n L946–953](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/app/src/i18n/en.ts#L946-L953)).
Its tool disclosure also refuses to open while the call is pending unless the tool opts in
([basic-tool.tsx L93, L179–182](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/session-ui/src/components/basic-tool.tsx#L179-L182)).
Orca's message list takes a "toolbar-driven desired open state for every tool run", but both production
entry points pass `false` and the prop is commented "legacy"
([NativeChatMessageList.tsx L84–85](https://github.com/stablyai/orca/blob/da6d483ab/src/renderer/src/components/native-chat/NativeChatMessageList.tsx#L84-L85),
[NativeChatStructuredSession.tsx L230](https://github.com/stablyai/orca/blob/da6d483ab/src/renderer/src/components/native-chat/NativeChatStructuredSession.tsx#L230)).
T3 Code has no global setting; instead it *remembers* each disclosure (turns, groups, entries,
reasoning) per thread across navigation
([MessagesTimeline.tsx L534–573, L1036–1042](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/chat/MessagesTimeline.tsx#L1036-L1042)).
Conductor's only documented global is the thinking toggle. Every app lets individual rows expand.

**Tool status is an icon, not a badge, in the shipping apps.** An expanded tool row's status: T3 Code
tints the tool icon, switches to an alert icon only for severe failures ("Reserve destructive row
styling for severe failures, not routine tool errors"), and its label verb changes (Running/Ran/Failed/
Declined/Stopped) ([MessagesTimeline.tsx L4727–4810](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/chat/MessagesTimeline.tsx#L4727-L4810));
subagent rows show a status word ("Working", "Failed · 12s · 4k tok") and settle to metrics when
successful ([L4582–4660](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/chat/MessagesTimeline.tsx#L4582-L4660)).
opencode shows failures as a separate error card whose title is the error's first clause
([tool-error-card.tsx](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/session-ui/src/components/tool-error-card.tsx)).
Superset's (flagged) chat is the one with an explicit status badge — `running`/`completed`/`failed`/
`declined`/`canceled`, destructive for failed/declined — plus a duration like "1.4s", and a header icon
per state (pulsing clock while running, green check, red X, orange denied); the row is open by default
only while running ([ToolCallRow.tsx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/desktop/src/renderer/routes/_authenticated/_dashboard/v2-workspace/%24workspaceId/hooks/usePaneRegistry/components/ChatV3Pane/components/Transcript/components/ToolCallRow/ToolCallRow.tsx),
[tool.tsx L62–75](https://github.com/superset-sh/superset/blob/2370f60d4/packages/ui/src/components/ai-elements/tool.tsx#L62-L75)).
Per-call duration appears only in Superset's chat and T3's subagent rows; per-turn duration is universal.

**Session list rows: a status glyph, a title, and git counts.** T3 Code resolves one pill per thread in
priority order — Pending Approval (amber) > Awaiting Input (indigo) > Working / Connecting (sky,
pulsing) > Plan Ready (violet) > Monitoring > Completed (emerald, only while unseen) — and shows a live
working duration, PR badges and a relative time; the list has Pinned, Active, Snoozed and Settled
sections, activity does not reorder it, and threads auto-settle after three days idle or on PR merge
([Sidebar.logic.ts L508–1045](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/Sidebar.logic.ts#L508-L533),
[thread-sidebar.md](https://github.com/pingdotgg/t3code/blob/95030dc67/docs/user/thread-sidebar.md#settle-finished-work)).
opencode's row is a single glyph (spinner working; warning dot for a permission; red dot for an unseen
error; blue dot for unseen activity) and the title
([sidebar-items.tsx L120–141](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/app/src/pages/layout/sidebar-items.tsx#L120-L141)).
Orca's glyphs are spinner / amber question mark ("Needs You") / emerald / red / gray; unread worktrees are
bolded, not badged; rows group by project
([agents-sessions.mdx](https://github.com/stablyai/orca/blob/da6d483ab/docs/site/content/docs/model/agents-sessions.mdx),
[worktrees.mdx](https://github.com/stablyai/orca/blob/da6d483ab/docs/site/content/docs/model/worktrees.mdx#sidebar-layout)).
Superset rows carry running agents and ports inline, ↑N/↓N ahead/behind, PR state, and a needs-attention
flag ([agent-status.mdx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/docs/content/docs/agent-status.mdx),
[workspaces.mdx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/docs/content/docs/workspaces.mdx#aheadbehind-status)).
Conductor's 0.44.0 sidebar shows the branch-status icon, title and +/− line counts per workspace, with a
hover card holding status ("In review"), author, age, summary, PR number and the next action ("Commit and
push") ([0.44.0](https://www.conductor.build/changelog/0.44.0-new-sidebar-rebuilt-composer-codex-checkpoints.md),
release screenshot). Several also offer a board by status as a *separate* view, not the sidebar:
Superset's Board (Idle / Working / Needs attention / Needs review / Merged / Deleted), Orca's
experimental Agent Dashboard (Needs You / Working / Done / Idle), Orca's Agents feed grouped by status
([activity.mdx](https://github.com/stablyai/orca/blob/da6d483ab/docs/site/content/docs/activity.mdx)).

**Work state lives in a side panel with several diff scopes, fetched live.** T3 Code's right panel
tabs are Diff, Files, Preview, Terminal, Pull request(s), Agents; the diff scopes to one turn's
checkpoint, unstaged, or the whole branch; below 980 px the panel becomes a sheet
([rightPanelStore.ts L22–33](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/rightPanelStore.ts#L22-L33),
[DiffPanel.tsx L192–222](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/components/DiffPanel.tsx#L192-L222),
[rightPanelLayout.ts](https://github.com/pingdotgg/t3code/blob/95030dc67/apps/web/src/rightPanelLayout.ts)).
opencode's review panel titles its scopes "Session changes", "Git changes", "Branch changes", "Last turn
changes" ([ui i18n L2–5](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/ui/src/i18n/en.ts#L2-L5)).
Conductor splits merge readiness into a **Checks** tab — git status, PR metadata, CI, deployments,
review threads, todos — beside the diff, and snapshots each turn as a checkpoint in a private git ref
([Checks](https://www.conductor.build/docs/reference/checks), [Checkpoints](https://www.conductor.build/docs/reference/checkpoints)).
Superset's diff focus mode jumps between "Against base, Commits, Staged, Unstaged"
([diff-viewer.mdx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/docs/content/docs/diff-viewer.mdx#focus-mode)).
None presents work state as "as of a moment"; all read the checkout live (T3 syncs PR state on a
cadence — [glossary.md](https://github.com/pingdotgg/t3code/blob/95030dc67/docs/internals/glossary.md#pull-requests)).
Unpushed work appears only as Superset's ↑N and Superset's bulk-delete warning about "dirty or unpushed
changes"; T3's per-turn checkpoint diff is the closest to kestrel's per-Run view.

**Only Conductor Cloud is multiplayer.** Workspaces in a Cloud organization are shared: "The transcript
and new agent output update live for everyone"; avatars show who has the workspace open; Conductor
"shows when they are typing in the same chat"; presence dots mark active teammates; the sidebar has
Pinned, My workspaces, Following and Team; a workspace can be followed or reassigned
([Multiplayer](https://www.conductor.build/docs/cloud/collaboration)). opencode's `/share` publishes a
public read-only link ([share.mdx](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/web/src/content/docs/share.mdx)).
T3 Code, Orca and Superset connect several of *one person's* devices to one host, not several people.
Several *agents* on one session is universal as subagents: T3's spawn rows and Agents panel, Orca's
subagent runs, Superset's subagents nested under their parent in the row's agents chip, opening a pane
that "follows its transcript live" ([Superset 13 Sep 2026 changelog](https://github.com/superset-sh/superset/blob/2370f60d4/apps/marketing/content/changelog/2026-09-13-pull-request-pane-subagents-new-agents.mdx)).

**Narrow screens and keyboard.** T3 Code documents the most: the right panel becomes a sheet at
≤980 px, a separate `max-sm` mobile path, `Cmd+1…9` for threads, `Cmd+K` palette,
`mod+Shift+Enter` to send the oldest queued message now, `mod+Enter` for the opposite follow-up
behavior ([keybindings.md](https://github.com/pingdotgg/t3code/blob/95030dc67/docs/user/keybindings.md)).
opencode has a "Bottom navigation" setting that moves the title bar and session tabs to the bottom on
mobile ([app i18n L940–942](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/app/src/i18n/en.ts#L940-L942)).
Orca and Superset answer small screens with separate iOS/Android apps rather than responsive layouts
([Orca mobile.mdx](https://github.com/stablyai/orca/blob/da6d483ab/docs/site/content/docs/mobile.mdx),
[Superset remote-access.mdx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/docs/content/docs/remote-access.mdx)).
Superset documents `⌘J` focus chat input, `⌘F` find in chat, `⌘L` changes tab, `⌘B` sidebar
([keyboard-shortcuts.mdx](https://github.com/superset-sh/superset/blob/2370f60d4/apps/docs/content/docs/keyboard-shortcuts.mdx)).
Conductor points to an in-app list (`⌘/`) and is a Mac app; its mobile app is listed as a Pro feature
with no layout documentation ([Keyboard shortcuts](https://www.conductor.build/docs/reference/keyboard-shortcuts)).

## Mapped to kestrel's Transcript

- *Detail* (tool calls) is collapsed by default everywhere, grouped per turn, with a live one-line label.
- *Narration* (thoughts) is either hidden behind a toggle (opencode, Conductor) or folded into the same
  activity row as detail with its own one-line preview (T3, Orca). No app separates narration from
  detail into different panes.
- *Shared state* — user messages, the final assistant message, approvals and questions — is what stays
  visible when a turn folds. Approvals and questions are pulled *out* of collapse: Superset keeps a group
  open while it holds an approval, Orca renders questions as cards "instead of treating it as a collapsed
  tool call", opencode hides a pending question tool from the turn and docks it at the composer
  ([session-turn.tsx L102–106](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/session-ui/src/components/session-turn.tsx#L102-L106),
  [session-question-dock.tsx](https://github.com/anomalyco/opencode/blob/b65de4d69/packages/app/src/pages/session/composer/session-question-dock.tsx)).
- A message waiting behind an active Turn is drawn beside the composer, and states its trigger.

## Not established

- Conductor's tool-call row (running/done/failed, duration, input/output) and its collapsed thinking:
  the docs say only that tool calls render in the transcript; no changelog screenshot shows one.
- Whether T3 Code, opencode or Orca show any per-*call* duration in an expanded tool row (not found).
- Conductor's behavior below Mac window widths.
- How Superset's terminal-first default shows tool activity beyond the terminal itself.

## Left undecided

This note does not choose kestrel's layout, whether a global expand setting should exist and at what
grain (all detail, per tool family as opencode does, or narration separately), whether queued messages
sit in the Transcript (T3, Orca) or in a tray on the composer (opencode, Conductor), whether a finished
Turn folds to "Worked for N", or which work-state scopes (per Run, per Turn, branch, Unpublished Work)
the inspector offers.
