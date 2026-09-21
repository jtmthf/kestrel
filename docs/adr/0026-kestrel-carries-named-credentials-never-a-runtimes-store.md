# kestrel carries named credentials, never a runtime's own store

A **Subscription Profile** carries a person's subscription access as things kestrel can name: a
variable, which reaches the Agent Runtime's environment, or a file beneath the agent's home that the
person names ([ADR-0025](0025-subscription-profiles-are-personal.md)). It does not carry an Agent
Runtime's own credential database, even when that is where the runtime keeps the login and refreshes
it. opencode 2 moved credentials into SQLite (`~/.local/share/opencode/opencode.db`), importing a
legacy `auth.json` once and writing refreshed OAuth tokens only there — so a Profile that names
`auth.json` under opencode 2 is a **seed**: the runtime imports it into a fresh database, and kestrel
does not read the refreshed token back. A runtime may add a named credential surface; kestrel will
not manufacture one by reading its store.

**Why not carry the database.** Two reasons, either sufficient. The store is not credentials:
opencode's database also holds the person's sessions, messages and projects, so carrying it would put
one person's runtime history into organization-held, encrypted storage and into every Run that names
the Profile, which a session's read boundary forbids. And reading it would make kestrel depend on
opencode's schema, the opencode-shaped assumption
[ADR-0007](0007-acp-is-the-agent-runtime-contract.md) exists to keep out.

## Considered options

- **Carry `opencode.db` as a Profile file.** Preserves token refresh; rejected above.
- **Add a Profile entry kind for an opaque store**, so the hand-back is atomic and WAL-aware. More
  honest than calling a database a file, but it rejects nothing: the data-boundary and
  schema-coupling objections remain, and the machinery is more than the decision deserves while the
  only such store is opencode's.
- **Keep `auth.json` named and accept the seed.** Chosen.

## Consequences

- An OpenCode Go or Zen subscription is a subscription-issued **key**, so it is unaffected: it is a
  variable (`OPENCODE_API_KEY`), and a key does not rotate.
- An opencode OAuth login supplied as `auth.json` works until the provider rotates the refresh
  token, after which the person logs in again and re-seeds. This is the shape ADR-0025 already
  accepts when a Run dies before handing back a refresh.
- Profiles gain no awareness of any runtime's paths. The image and `USAGE.md` lead with the
  variable; the file is documented as a seed.
- Durable rotating-login support, if wanted, is requested of the runtime as a nameable credential —
  not retrofitted here.
