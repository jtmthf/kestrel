## Agent skills

### Issue tracker

Issues are tracked in GitHub (`jtmthf/kestrel`). See `docs/agents/issue-tracker.md`.

### Triage labels

Default five canonical labels (needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context layout (root `CONTEXT.md` + `docs/adr/`). See `docs/agents/domain.md`.

## Code

### Comments

**The default is no comment.** Write one only where a reader would otherwise get it wrong:
a trap, a rejected alternative, a constraint an ADR imposes, a consequence that is invisible
at the call site. One sentence. Prefer a better name to a comment explaining a worse one.

Delete on sight:

- Restatements of the code, including doc comments that paraphrase the signature below them.
- Roadmap narration (`At 0.1 it starts and waits; the link arrives in 0.1/04`). The issue
  tracker holds the plan; the code holds what is true now.
- Module docs that repeat the module's name.

Ten comments in a codebase is a reasonable number. Assume a comment is unnecessary and make
it argue for itself.
