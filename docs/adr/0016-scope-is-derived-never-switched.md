# Scope is derived, never switched

kestrel ships no command that changes which Organization subsequent commands apply to. There is no
`kestrel config use-context`, no active profile, and no stored current organization. Scope resolves
per invocation, in this order: the `--organization` flag, then `KESTREL_ORGANIZATION`, then a binding
derived from the working directory, then the only Organization that exists.

Eighteen subcommands take `--organization`, and the obvious fix is the one kubectl, gcloud and aws all
shipped: a context you set once and forget. This records why kestrel does not, because the fix is easy
to add later and nearly impossible to remove once scripts depend on it, and because refusing what
three major CLIs do will otherwise read as an oversight somebody should correct.

## Why not a switchable context

A global mutable context is state that is invisible until it is wrong, and the command that reads it
is usually the one you did not want to run against it. The evidence that this is unsolved rather than
merely disliked is that a third-party guardrail ecosystem exists to compensate for it —
[`kubesafe`](https://github.com/Telemaco019/kubesafe) marks contexts as safe and protects commands
against the others, and the standard community advice is to put the current context in your shell
prompt, which concedes that the fix lives outside the tool.

Doppler's inversion scores better precisely because its scope is *derived from where you are
standing* rather than set and remembered: you cannot forget to switch a thing you never switched. That
is the model here, and kestrel's `Organization` is unusually well suited to it, because a Workspace is
already repositories and a branch — the working directory is not a proxy for the scope, it is very
nearly the scope itself.

There is an ergonomic tell as well. `gcloud config configurations activate` is long enough that users
alias around it. A scope-switch command that people alias is a command that should not have existed.

## Consequences

- **`--organization` is one uniform flag, inherited by every scoped command**, never `--org` in one
  place and `--organization` in another. Anything kestrel infers must be overridable by exactly one
  flag, spelled one way.
- **kestrel ships a provenance command** that prints each resolved value and where it came from. It
  is the single best mitigation for "why is this command doing that", and almost nobody ships one.
- **Implicit resolution applies only while exactly one Organization exists.** The flag becomes
  required again the moment there are two, which is the rung where multi-tenancy becomes a capability
  — no migration, no deprecation, and no window in which an unqualified command can land in a tenant
  nobody named.
- **A directory binding, if one is added, is a committed file rather than a hidden one.** The scope
  belongs in the pull request, which is the same reason
  [#86](https://github.com/jtmthf/kestrel/issues/86) already puts Trigger declarations in version
  control.
