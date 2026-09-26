# A Run spans prompt Turns

A Run owns one continuing ACP conversation and may receive several prompt Turns. Follow-up
instructions that arrive during a Turn wait durably and are sent to that same conversation after
the Turn completes. A Turn's completion reports its response but does not end the Run; the Run ends
on explicit stop, Session seal, or failure. This replaces the disposable-per-Run assumption and
single-Turn reporting in [ADR-0007](0007-acp-is-the-agent-runtime-contract.md), while retaining
its one-ACP-session-per-Run rule. A lost ACP process is resumed only when that runtime supports it;
otherwise the Run fails visibly and the next instruction starts a new Run, not a fictitious
continuation.

A Run waiting between Turns retains its ACP process and Instance but releases its active-work
slot. An Approval still holds that slot. Thus the one-Run-per-Session rule of
[ADR-0014](0014-concurrency-lives-across-sessions-never-within-one.md) remains, while its claim
that no Environment sits idle no longer describes a waiting conversation. The Instance still
follows [ADR-0018](0018-an-instance-lives-until-its-session-seals.md): it is not reaped while it
holds Unpublished Work, even if the one-day idle deadline passes. A pull request is neither a
required result nor the Run's end condition; research, CI repair, and conflict resolution can
finish without one. A final Outcome is recorded separately from Turn responses and is not posted
as a duplicate success comment.

A waiting instruction is editable by its author until a Turn takes it, and a person may interrupt
the active Turn through ACP's `session/cancel` without ending the Run.
