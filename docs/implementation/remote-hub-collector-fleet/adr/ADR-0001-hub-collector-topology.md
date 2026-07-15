# ADR-0001: Hub/Collector Topology

- Status: accepted
- Date: 2026-07-14
- Canonical stream: `dirtydash-px3`

See also: [`CONTEXT.md`](../CONTEXT.md), [`/api/v1` Protocol And Privacy Invariants`](../API_V1_INVARIANTS.md)

## Context

Dirtydash previously carried an agentless SSH-pull roadmap. The accepted stream replaces that design because canonical usage, privacy enforcement, durable offline delivery, and fleet administration all become simpler when parsing stays on each machine and normalized metadata is pushed to one Hub.

## Decision

Dirtydash uses a push-based topology:

- one self-hosted Hub owns the canonical database, dashboard, and fleet control plane;
- every Machine, including the Hub host, runs one outbound-only Collector;
- Collectors parse locally, normalize metadata locally, and push authenticated `/api/v1` batches to the Hub;
- `dirtydash serve` remains loopback-only and account-free for the all-in-one local experience;
- agentless SSH pull is superseded as the fleet transport direction.

## Consequences

- Fleet administration, enrollment, and diagnostics are centered on Machine and Collector state rather than remote file scraping.
- No inbound Collector ports or remote daemons are required.
- Hub freshness depends on durable at-least-once delivery, local Collector manifests, and idempotent Hub ingestion.
- Historical SSH-pull documents remain in the repository for context, but they are no longer an active implementation roadmap.
