# Phase 7: Migration, Backup, and Release Hardening

Canonical Beads issue: `dirtydash-px3.7`

Epic: `dirtydash-px3`

Status is tracked in Beads. This document preserves accepted intent and is decision-complete, implementation-open.

## Outcome

Existing Dirtydash history migrates safely, backups and restore are operationally proven, legacy pull has a bounded deprecation path, and the complete fleet passes real multi-platform release acceptance.

## Why This Phase Exists

The product is not releasable until existing users can seed history without duplicates and operators can recover from restart, data loss, and failed updates.

## Scope

Allowed:

- Consistent local-database upload during first Hub deployment.
- Idempotent Collector backfill after seeding.
- Verified daily backups retaining seven daily snapshots and the three latest pre-upgrade snapshots, plus download, restore, and configurable retention.
- One-release deprecation of legacy `remote` pull commands with migration guidance.
- Real-tailnet gate with one Linux Hub, another Linux Collector, and one macOS Collector.

Out of scope:

- Indefinite compatibility with legacy pull commands.
- Unverified backup files presented as recoverable.

## Constraints

- Seed and backfill use the same stable identities and never duplicate usage.
- Backups must be verified and restore-tested.
- Release proof covers restart, offline replay, privacy, and failed-update rollback.

## Settled Decisions

Retention defaults are seven daily plus three pre-upgrade snapshots. Legacy remote pull remains for one compatibility release only as a bounded deprecation path for historical commands; it is not an active implementation roadmap.

## Open Questions

None.

## Dependencies

- Depends on: `dirtydash-px3.6`
- Parallel-safe: no.

## Acceptance Evidence

- Existing local history seeds without manual imports and backfill deduplicates.
- Backup verification, download, retention, restore, restart, and rollback tests pass.
- The real-tailnet topology demonstrates normal ten-second freshness, offline recovery without duplication, metadata-only persistence, and all required OS roles.

## Quality Gates

- `cargo test`
- `npm --prefix dashboard run build`
- Migration, backup, restore, seed/backfill, deprecation, and rollback tests.
- Full real-tailnet release checklist with retained evidence.

## Replanning Triggers

Replan if seed plus backfill cannot be proven idempotent, backup restore cannot meet durability requirements, or the real-tailnet topology fails latency, privacy, restart, or rollback acceptance.

## Implementation Hypotheses

These are suggestions to validate against repository evidence, not mandatory choreography.

- Produce the seed from a consistent SQLite backup rather than copying live files.
- Record verification metadata beside each backup and refuse unverified restore by default.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
