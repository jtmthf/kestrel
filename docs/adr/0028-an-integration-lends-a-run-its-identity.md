# An Integration lends a Run its identity

A Run that works on a GitHub repository reaches GitHub as the **Integration** that watches that
repository, never as a person. The GitHub Integration is a GitHub App each installation of kestrel
creates for itself; the control plane holds its private key and mints short-lived installation
tokens from it. The agent's `gh` and `git` ask for a token each time they reach GitHub, and the
token is scoped to the Workspace's repositories. What the Integration's own identity says on
GitHub, kestrel's comments and its agents' alike, is never taken as input or as a command.

**Why identity, not a filter.** #123 found the failure this closes: an agent given the operator's
token as `GH_TOKEN` commented on its originating issue, the comment passed a Trigger that trusted
the operator's login, and the Session queued a Run to answer its own agent. kestrel's marker kept
its own comments out, but a comment the agent wrote directly carried none, and kestrel cannot tell
two writers apart who authenticate as one account. Filtering the credential's owner while a Run is
active would deafen the Session to the operator it serves; asking agents not to comment holds only
as long as every brief remembers to say so. Only a second identity separates them, and the one
kestrel can create, hold and recognise without the operator provisioning anything is an App's.

**Per use, not per spawn.** An installation token expires after an hour, and a Run spans turns
([ADR-0024](0024-a-run-spans-prompt-turns.md)) in one agent process, so a token placed in that
process's environment at the spawn would die inside long Runs. This amends
[ADR-0010](0010-a-provider-credential-crosses-the-link-at-the-spawn.md) for credentials an
Integration issues: the image's `gh` is a shim and `git` has a credential helper, each asking the
supervisor, which asks the control plane over the link, for a current token, and handing it only to
the process that is about to use it. It is still never in a file or in the Environment's
environment. A Provider Credential keeps crossing once, at the spawn.

**The Workspace's repository chooses the Integration**, not the Event that opened the Session: a
scheduled or dispatched Session opens pull requests too. Today an Integration watches exactly one
repository, so the rule is a match on that repository; how one installation serves many
repositories is #140's to settle.

**An Integration never hears itself, by author.** A remark or a command whose author is the
Integration's own login feeds no Session and fires no Trigger, whatever a Trigger's filter admits.
A filter that admits `CONTRIBUTOR` may well admit the App's bot, so this cannot be left to filters.
The marker kestrel writes into its comments stays, for the one thing only it can do: finding a
comment kestrel already delivered.

## Considered options

- **A personal access token, the operator's or a machine user's.** The operator's is the bug. A
  machine user costs a second account and a seat, and GitHub does not let a fine-grained token work
  on a personal repository the account only collaborates on, so it means a classic token.
- **One App shared by every kestrel.** Minting its tokens needs its private key, so either every
  installation holds a key that reaches every other installation's repositories, or its maintainer
  runs a token broker that sits in every installation's trust and availability path. The manifest
  flow gives each installation its own App in one click instead.
- **Keep a token path beside the App**, for development and CI. It keeps #123 as a supported mode.
  The tests stub GitHub, and stubbing token minting is no harder than stubbing a token.
- **Mint at the spawn and accept the expiry.** Runs longer than an hour would lose GitHub
  mid-push.
- **An authenticating proxy** that adds the token to the Instance's GitHub traffic, so the agent
  never holds one. The stronger boundary, and the likely destination, but `gh` and `git` over HTTPS
  need interception or a custom host to use it. Deferred, not rejected.
- **Recognise kestrel's voice by the marker, or by marker and author.** Any comment that quotes a
  marker would pass for kestrel's.

## Consequences

- An operator creates the App from kestrel before registering a GitHub Integration; there is no
  token to hand kestrel and no `GH_TOKEN` to set.
- A token carries every permission the App was granted, which includes merging: a token that can
  push the Run's branch can merge it. Narrowing permissions per Run is Policy, and Policy is `0.4`.
- Everything kestrel and its agents say on GitHub appears as the App, so a person reading an issue
  can tell them from its maintainer too.
