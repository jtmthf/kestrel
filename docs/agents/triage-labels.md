# Triage Labels

The skills speak in terms of five canonical triage roles and a difficulty vocabulary. This file maps both to the actual label strings used in this repo's issue tracker.

| Label in mattpocock/skills | Label in our tracker | Meaning                                  |
| --------------------------- | --------------------- | ----------------------------------------- |
| `needs-triage`              | `needs-triage`        | Maintainer needs to evaluate this issue  |
| `needs-info`                | `needs-info`          | Waiting on reporter for more information |
| `ready-for-agent`           | `ready-for-agent`     | Fully specified, ready for an AFK agent  |
| `ready-for-human`           | `ready-for-human`     | Requires human implementation            |
| `wontfix`                   | `wontfix`             | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the corresponding label string from this table.

Edit the right-hand column to match whatever vocabulary you actually use.

## Difficulty labels

A ticket an agent will pick up carries one difficulty label. It rates how hard the work is — a single axis. The model and the reasoning effort are two dials the dispatcher turns separately to meet it, and neither is recorded on the ticket.

| Label | Difficulty |
| ------------------------ | ------------------------------------------------------------------ |
| `difficulty:trivial` | Mechanical and fully specified; no design judgment. |
| `difficulty:easy` | Small and clear; a little judgment, bounded blast radius. |
| `difficulty:moderate` | Ordinary feature work. The rung a ticket takes when it carries none. |
| `difficulty:hard` | Subtle or cross-cutting; design judgment, wider blast radius. |
| `difficulty:extreme` | Ambiguous or high-risk; may reshape the architecture. |

`issue-tracker.md` holds the mechanics: applying, revising, and querying a rung, and what it means for a model and an effort.
