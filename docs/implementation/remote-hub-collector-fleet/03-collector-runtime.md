# Phase 3: Collector Runtime

Canonical Beads issue: `dirtydash-px3.3`

Epic: `dirtydash-px3`

Status is tracked in Beads. This document preserves accepted intent and is decision-complete, implementation-open.

## Outcome

A durable outbound-only Collector parses supported local harness data, redacts it to the accepted metadata contract, and delivers it reliably through online, offline, restart, and parser-upgrade conditions.

## Why This Phase Exists

Fleet freshness and privacy depend on moving parsing and provenance handling to each machine while making delivery durable and idempotent.

## Scope

Allowed:

- Shared local parsing pipeline behind existing importers.
- File manifests, source provenance, watchers with debounce, complete fifteen-minute reconciliation, durable SQLite outbox, retry backoff, and twenty-second command long-poll.
- Owner commands for refresh, credential rotation, diagnostics, and approved updates.
- Real fixtures for Claude Code, Codex, OpenCode, Pi, and Hermes, including malformed records and parser upgrades.
- Display-safe project names, salted project identifiers, confidence, pricing version, and metadata redaction.

Out of scope:

- Inbound Collector ports.
- Hub deployment automation or fleet management UI.
- Raw conversation or absolute-path transport.

## Constraints

- Delivery is at least once; unacknowledged normalized events persist locally.
- Watchers are best-effort and never replace full reconciliation.
- Raw session content and absolute paths never enter request payloads.
- The Collector manifest and outbox remain local SQLite databases.

## Settled Decisions

All five named Agents are V1 inputs; stable event identity is Machine plus Agent plus fingerprint; the Hub acknowledges only fully committed batches.

## Open Questions

None.

## Dependencies

- Depends on: `dirtydash-px3.2`
- Parallel-safe: no.

## Acceptance Evidence

- Importer fixtures cover all five Agents, malformed records, token categories, confidence, redaction, and parser upgrades.
- Offline and retry tests prove eventual deduplicated arrival.
- Payload assertions prove prohibited content and paths never leave the machine.
- Watcher failure visibly degrades to periodic and manual reconciliation.

## Quality Gates

- `cargo test`
- Focused importer, manifest, outbox, retry, command-delivery, redaction, and reconciliation tests.

## Replanning Triggers

Replan if a supported harness cannot provide stable metadata-only event identity or file watching cannot degrade safely to reconciliation.

## Implementation Hypotheses

These are suggestions to validate against repository evidence, not mandatory choreography.

- Preserve current importer parsing logic behind a shared Collector-facing interface.
- Treat file notifications as hints that schedule the same incremental reconciliation path.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
