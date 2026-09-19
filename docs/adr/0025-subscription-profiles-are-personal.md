# Subscription Profiles are personal

A person's Subscription Profile persists outside task Instances and authorizes only Runs that
person permits. It is distinct from the Organization's Provider Credentials in
[ADR-0010](0010-a-provider-credential-crosses-the-link-at-the-spawn.md). The dogfooding path
cannot depend on separately provisioned metered model-provider API accounts: Codex uses ChatGPT
account access, OpenCode Go uses its subscription-issued key, and Claude subscription access
remains conditional on resolving the vendor's conflicting third-party entitlement guidance.

The profile's secret is never baked into a development image, repository, transcript, or issue.
Persistent private storage is necessary because a runtime may refresh its login across Runs.
Until concurrent refresh behavior is proven safe, one personal Codex profile serves at most one
active Codex Run at a time. This deliberately rejects treating subscription access as another
Organization-wide Provider Credential, which would share a person's account across unrelated
work and lose the ownership boundary.

## How it is carried

**A profile is sealed in kestrel's database and crosses the link at the spawn**, like a Provider
Credential ([ADR-0010](0010-a-provider-credential-crosses-the-link-at-the-spawn.md)), and is handed
only to the Runs of a Session that names it. The Session records its profile when it opens, from
`session open --profile` or from its Trigger's declaration, and a follow-up continuation keeps it.
A profile holds variables, which reach the Agent Runtime's environment, and files, which the
supervisor writes beneath the agent's home. When the runtime exits, the supervisor hands back every
file that changed, then removes them all, and only then reports the Run finished. The report ends
the Run and with it the Run's right to hand anything back. Only files the profile already holds are
taken back, so a Run can refresh a login and never add one. kestrel holds no table of where each
runtime keeps its login: the person names the path.

We considered a Docker volume per profile, mounted where each runtime keeps its login, and rejected
it. A refresh would persist with no hand-back, but the volume works only on the Docker driver, stays
mounted while an Instance idles between Runs, and needs the filtered daemon proxy
([ADR-0009](0009-the-daemon-is-reached-through-a-filtered-proxy.md)) to allow volume creation.

**One profile serves one Run at a time on a runtime whose login rotates.** The work role takes a
list of such runtimes, `codex` by default. It leaves a queued Run on one of them waiting while
another Run on the same runtime with the same profile is active. Runs on other runtimes, and Runs
with other profiles, are dispatched as usual.

## Consequences

- **A profile's files are on the Instance's disk for the length of a Run.** A runtime reads its
  login from a file, so this is the floor, unlike a Provider Credential. The agent and anything it
  runs can read them, as they can read its environment.
- **A Run that dies before handing back loses the refresh.** A supervisor that is killed, or whose
  lease expires, never hands back what the runtime rewrote. For a rotating login, the next Run then
  starts from a token the refresh may have revoked, and the person logs in again. The files it
  wrote also stay on that Instance until the next Run overwrites them or the Instance is destroyed.
- **The local-exec driver gives each Instance a home of its own**, beside its working directory.
  Otherwise a profile's files would be written into, and removed from, the operator's own home.
- **Naming a profile is the only authorization there is.** The operator boundary authenticates
  nobody ([ADR-0015](0015-the-cli-is-a-client-not-a-role.md)), so whoever can declare a Trigger or
  open a Session can spend a person's subscription. The owner is recorded on the profile and never
  changes, but nothing checks who named it.
