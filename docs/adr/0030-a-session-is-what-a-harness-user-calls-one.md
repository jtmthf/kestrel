# A Session is what a harness user calls one

kestrel's words now mean what someone arriving from Claude Code, Cursor or the
[Dictionary of AI Coding](https://github.com/mattpocock/dictionary-of-ai-coding) already expects.
What was a **Run** — one harness execution, one ACP conversation, one context — is now a
**Session**. What was a **Session** — the durable, joinable container holding many of them — is
now a **Workspace**. What was a **Workspace** — the declaration of repositories and their
defaults — is now a **Project**. **Agent Runtime** is now **Harness**.

"Session" was the costliest divergence. Across harnesses and the products built on them, a session
is one conversation with an agent and its own context; kestrel used the word for the thing one level
up, so every reader carrying the common meaning misread every sentence containing it. The shape
kestrel has is the one a worktree-based developer already works in: a worktree holds the branch and
the checkout, and several harness sessions — the main work, then fresh contexts for review, merge
conflicts and CI failures — happen inside it. Workspace names the worktree; Session names the
harness session.

ADRs 0001 to 0029 are written in the earlier names and are not rewritten.

## Considered options

**Rename only the top, keeping Run.** A Thread-contains-Runs pair has precedent in the OpenAI
Assistants API, and Linear's `AgentSession` spans many prompts the way kestrel's old Session did.
Rejected: it left "session" either banned from a product about agent sessions or pointing at the
container, and kestrel translates no integration's vocabulary, so Linear's word binds only its
adapter.

**Thread**, **Task** or **Worktree** for the container. Thread names a conversation rather than a
place work lives; Task names the work rather than the place; Worktree is git's word, while a
workspace may hold many repositories and kestrel runs no git command.

**Fold the Environment into the Project.** Rejected: the same repositories must be reusable across
workspaces with different toolchains, so a workspace names its Project and its Environment
separately.
