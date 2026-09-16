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
