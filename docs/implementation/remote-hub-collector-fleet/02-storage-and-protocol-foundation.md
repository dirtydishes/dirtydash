# Phase 2: Storage and Protocol Foundation

Canonical Beads issue: `dirtydash-px3.2`

Epic: `dirtydash-px3`

Status is tracked in Beads. This document preserves accepted intent and is decision-complete, implementation-open.

## Outcome

The Hub has a versioned, authenticated, metadata-only ingestion contract and a durable SQLite repository foundation that safely accepts concurrent Collector traffic.

## Why This Phase Exists

Collector delivery, enrollment, fleet operations, and the redesigned Usage surface all depend on stable identity, transaction, query, and access contracts.

## Scope

Allowed:

- Repository interfaces around transactions and usage queries.
- SQLite WAL migrations for Machines, credentials, ingest batches, checkpoints, sync runs, source manifests, owner sessions, and backup metadata.
- Idempotent transactional `/api/v1` batch ingestion, Machine identity, credential rotation/revocation, UTC storage, owner-time-zone aggregation, Argon2id administrator login, and separate trust handling for Tailscale/private and public listeners.
- Existing read APIs for one compatibility release.

Out of scope:

- PostgreSQL adapter implementation.
- Collector file watching, deployment automation, fleet UI, or Usage redesign.

## Constraints

- Stable identity includes Machine ID, Agent, and Collector fingerprint.
- Entire batches commit before acknowledgement; ingestion writes are serialized.
- Public listeners ignore Tailscale identity headers.
- Collector credentials are stored hashed; browser sessions are secure and CSRF-aware.

## Settled Decisions

SQLite WAL is canonical V1 storage; timestamps are UTC and calendar days use one owner-selected time zone defaulted from setup browser; `/api/v1` is the versioned protocol boundary.

## Open Questions

None.

## Dependencies

- Depends on: `dirtydash-px3.1`
- Parallel-safe: no.

## Acceptance Evidence

- Protocol tests cover duplicates, partial failures, retries, credential rotation, incompatible versions, and concurrent Collectors.
- Security tests reject forged Tailscale headers and validate CSRF/session and revocation behavior.
- Database tests cover WAL concurrency, repository boundaries, migrations, and DST/time-zone rebucketing.

## Quality Gates

- `cargo test`
- Focused API, repository, migration, authentication, security, and time-zone tests.

## Replanning Triggers

Replan if serialized SQLite ingestion cannot satisfy concurrent Collector requirements or the listener design cannot isolate Tailscale and public authentication.

## Implementation Hypotheses

These are suggestions to validate against repository evidence, not mandatory choreography.

- Introduce the repository seam around existing SQLite query boundaries before expanding tables.
- Keep compatibility adapters thin and explicitly deprecated.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
