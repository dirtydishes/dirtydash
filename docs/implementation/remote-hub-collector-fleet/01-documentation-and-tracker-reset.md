# Phase 1: Documentation and Tracker Reset

Canonical Beads issue: `dirtydash-px3.1`

Epic: `dirtydash-px3`

Status is tracked in Beads. This document preserves accepted intent and is decision-complete, implementation-open.

## Outcome

The repository and tracker consistently describe Dirtydash as local plus self-hosted, using the accepted Hub/Collector language and explicit protocol, privacy, authentication, and storage boundaries.

## Why This Phase Exists

The previous roadmap assumed agentless SSH pull. Implementation cannot safely build the replacement system while old terms and security assumptions remain authoritative.

## Scope

Allowed:

- Supersede the old remote-pull epic and children.
- Add `CONTEXT.md` with Hub, Collector, Machine, Agent, Source Record, Usage Event, and Sync Run.
- Add ADRs for Hub/Collector topology, metadata-only privacy, Tailscale-plus-fallback authentication, and the SQLite repository seam.
- Update product positioning and define `/api/v1` protocol/privacy invariants.

Out of scope:

- Runtime storage migrations, Collector services, deployment code, or UI implementation.
- Changing accepted topology or privacy decisions.

## Constraints

- Tracker schema reconciliation uses the designated migrator only.
- Keep historical remote-pull artifacts for context; supersede rather than erase history.
- Documentation must distinguish product/domain decisions from implementation hypotheses.

## Settled Decisions

The canonical terms, topology, privacy boundary, access modes, SQLite V1 posture, and local `serve` behavior in `IMPLEMENT.md` are settled.

## Open Questions

None.

## Dependencies

- Depends on: none
- Parallel-safe: no; this establishes language and contracts for every later phase.

## Acceptance Evidence

- Beads shows `dirtydash-px3` as the replacement for `dirtydash-refresh-loop` and its obsolete children.
- `docs/implementation/remote-hub-collector-fleet/CONTEXT.md`, `docs/implementation/remote-hub-collector-fleet/adr/`, product copy, and `docs/implementation/remote-hub-collector-fleet/API_V1_INVARIANTS.md` are cross-consistent.
- No active roadmap still directs implementation toward agentless SSH pull.

## Quality Gates

- Documentation link and terminology scan.
- `cargo test` and dashboard build only if executable or generated product surfaces change.

## Replanning Triggers

Replan if the accepted Hub/Collector terminology or metadata-only boundary proves internally inconsistent.

## Implementation Hypotheses

These are suggestions to validate against repository evidence, not mandatory choreography.

- Reuse the repository's existing ADR/domain documentation conventions if present.
- Link superseded docs forward to this stream instead of rewriting their history.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
