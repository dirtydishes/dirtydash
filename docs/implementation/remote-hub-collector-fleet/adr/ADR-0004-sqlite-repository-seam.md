# ADR-0004: SQLite Repository Seam

- Status: accepted
- Date: 2026-07-14
- Canonical stream: `dirtydash-px3`

See also: [`CONTEXT.md`](../CONTEXT.md), [`/api/v1` Protocol And Privacy Invariants`](../API_V1_INVARIANTS.md)

## Context

Dirtydash already uses SQLite locally. The accepted fleet stream keeps SQLite for V1 so the product stays operable and debuggable, but the Hub still needs a narrow seam around persistence so ingestion, queries, migrations, and future storage experiments do not leak throughout the codebase.

## Decision

Dirtydash V1 Hub storage remains SQLite in WAL mode behind an explicit repository boundary.

That seam must:

- own transactional writes and query interfaces;
- serialize Hub ingestion writes;
- isolate schema details from protocol and UI code; and
- keep PostgreSQL or other future storage work deferred rather than preemptively implemented.

## Consequences

- Phase 2 starts by carving repository interfaces around existing persistence work instead of spreading direct SQL access.
- Tests can target repository behavior, time-zone aggregation, and migration correctness without depending on higher layers.
- SQLite performance or concurrency limits become explicit replanning triggers rather than silent assumptions.
- A future storage backend would be a new decision built behind the same seam, not an implicit V1 commitment.
