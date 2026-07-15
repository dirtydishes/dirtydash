# ADR-0002: Metadata-Only Privacy Boundary

- Status: accepted
- Date: 2026-07-14
- Canonical stream: `dirtydash-px3`

See also: [`CONTEXT.md`](../CONTEXT.md), [`/api/v1` Protocol And Privacy Invariants`](../API_V1_INVARIANTS.md)

## Context

Dirtydash is meant to be trustworthy for developers who want usage inspection without handing over session content. A fleet architecture only remains aligned with that posture if the Collector-to-Hub contract forbids raw content and dangerous identifiers from persistent transport and storage.

## Decision

Dirtydash fleet transport and Hub persistence are metadata-only.

Specifically:

- Collectors may read local raw session artifacts to parse them, but that access stays on the local Machine.
- `/api/v1` batches and Hub persistence exclude raw prompts, raw responses, copied session bodies, and absolute paths.
- SSH passwords, sudo passwords, and similar deployment secrets never enter persistent storage.
- Usage records keep only the provenance, identifiers, confidence, pricing, and timing metadata needed for inspection, deduplication, and troubleshooting.

## Consequences

- Privacy review applies to both payload design and database schema design.
- Collector manifests may retain local machine details needed for reconciliation, but Hub-visible source metadata must be redacted or non-reversible.
- Features that require raw content reconstruction are out of scope unless a new ADR explicitly changes the boundary.
- Violations of this boundary are architecture bugs, not optional product tradeoffs.
