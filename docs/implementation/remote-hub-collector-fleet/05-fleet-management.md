# Phase 5: Fleet Management

Canonical Beads issue: `dirtydash-px3.5`

Epic: `dirtydash-px3`

Status is tracked in Beads. This document preserves accepted intent and is decision-complete, implementation-open.

## Outcome

Owners can understand, refresh, repair, rotate, update, archive, and deliberately delete Machines without losing fleet history or turning one failed node into a fleet-wide outage.

## Why This Phase Exists

Deployment creates machines; durable operation requires health, compatibility, credential, repair, update, and lifecycle controls.

## Scope

Allowed:

- Machines surface and enrollment progress.
- Online, syncing, stale, offline, update-available, and action-required states with text and iconography.
- Per-Machine refresh, credential rotation, repair, archive, separate typed-confirmation deletion, compatibility status, and signed updates.
- Snapshot-first update fleet flow: Hub first, health check, then individual Collectors with independent rollback.

Out of scope:

- Usage ledger redesign.
- Mobile enrollment, credentials, deployment, or destructive actions.

## Constraints

- Removing a Machine revokes its Collector and archives history; deletion is separate.
- Hub protocol supports current and previous Collector versions.
- Failed nodes roll back independently.

## Settled Decisions

Only Machines are manually added. Machine state is never communicated by color alone. Tablet or desktop is required for administrative actions.

## Open Questions

None.

## Dependencies

- Depends on: `dirtydash-px3.4`
- Parallel-safe: no.

## Acceptance Evidence

- A Machine can be added entirely through the hosted UI using Hub-side SSH.
- Rotation, revocation, repair, archive, and deletion tests preserve their distinct semantics.
- Fleet update tests snapshot and update the Hub first, tolerate current/previous protocol versions, and independently roll back failures.

## Quality Gates

- `cargo test`
- `npm --prefix dashboard run build`
- Focused credential, compatibility, update, rollback, accessibility, and browser workflow tests.

## Replanning Triggers

Replan if current/previous Collector compatibility cannot support staged updates or independent node rollback cannot preserve Hub availability.

## Implementation Hypotheses

These are suggestions to validate against repository evidence, not mandatory choreography.

- Derive displayed health from explicit timestamps, protocol state, service diagnostics, and update compatibility rather than one opaque enum.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
