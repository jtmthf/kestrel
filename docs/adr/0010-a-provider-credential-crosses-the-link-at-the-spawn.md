# A Provider Credential crosses the link at the spawn

The secret an agent reaches a model provider with belongs to the **Organization**. It is encrypted
in kestrel's database, decrypted only when an **Environment** says it is about to spawn its **Agent
Runtime**, carried over the link for that one **Run**, and put into that process's environment.
It is never in the Environment's own environment, never in a file, and never in a **Transcript**.

**ACP carries none of this, and needs no feature for it.**
[ADR-0007](0007-acp-is-the-agent-runtime-contract.md) makes kestrel an ACP *client*, and the client
derives the agent's command from its own configuration — so the process the supervisor spawns is
the supervisor's to configure, and a key in that process's environment is the whole mechanism. ACP's
`authenticate` and `authMethods` are a different thing: they are subscription logins, and an agent
offering only `terminal` launches an interactive process for a human who is not there, which is
refused at `initialize` rather than attempted.

**At the spawn, not at provision.** The Environment could be handed the key with the four variables
it already gets. It is not, because an Environment outlives the turn at both ends: it is provisioned,
a Workspace is checked out into it, a turn is worked, and it is destroyed. A variable set at
provision is in `docker inspect`, in `/proc/<pid>/environ` for every process in the Environment, and
in the Environment for its whole life, including while it is idle and while it is being checked out.
Asking for it at the spawn narrows the window to the turn and the reader to one process. What an
idle Environment holds is then nothing, which is a property a test can watch: an Environment that is
never told to start never asks, and the control plane never decrypts.

**The encryption key is generated at first boot and stored beside the database.** `kestrel.key` in
the data directory, 32 bytes, mode `0600`, written the first time kestrel opens a data directory.
An operator supplies provider keys and never a key of kestrel's, which is what keeps the
zero-required-configuration rule. Each credential is sealed with XChaCha20-Poly1305 under a fresh
nonce, with `<organization>/<variable>` authenticated alongside it, so a sealed value moved to
another organization's row or another variable's no longer opens.

**A credential is named by the variable a runtime reads it from.** `kestrel credential set
ANTHROPIC_API_KEY --organization acme`, with the secret on standard input rather than in an
argument, where a shell history and `ps` would both have it. kestrel holds no table of providers and
no opinion about which of them an Agent Runtime speaks to: the operator names the variable, and the
runtime finds it where it always looks.

**A Run with no way to reach a model fails before an Environment.** An Organization holding no
credential, driving a runtime kestrel was configured with no ACP login for, cannot work: the Run
ends failed at dispatch, naming the organization, rather than provisioning an Environment to
discover it inside a model call.

## Consequences

- **The key file is the backup boundary, and it is easy to get wrong.** A copy of `kestrel.db`
  without `kestrel.key` holds no usable credential, which is the point; an operator who backs up
  one and not the other restores an install whose credentials must all be set again. Both are in
  the data directory, and the compose path puts that on one volume, so the default does the right
  thing and a hand-rolled backup of the database alone does not.
- **Rotation reaches the next Run, never the one in flight.** A credential is read once, as the
  agent is spawned. Replacing it does not reach a turn already being worked, and there is nothing
  that revokes one from a running agent — the Run ends, and the next one is spawned with the new
  value.
- **An Organization is the only scope there is.** Every Run in an Organization is spawned with
  everything that Organization holds. Narrowing a credential to an Agent, a Workspace or a
  Workflow is a boundary nothing yet needs, and `CONTEXT.md` says an Agent does not hold one.
- **The supervisor holds a secret in memory for the length of the turn.** It is in the supervisor's
  process and in the agent's, and a core dump of either would carry it. That is the floor for
  anything that spawns a process with a key: what this buys over the alternative is that neither
  the Environment nor the disk under it ever has one.
- **The link grew a fourth operation.** `GET /link/runs/{run}/credentials`, authenticated as the Run
  like the other three, and refused once the Run has ended because the Run's credential is
  invalidated then.
