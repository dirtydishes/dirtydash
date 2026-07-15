# Context: Dirtydash Remote Hub and Collector Fleet

Canonical scope: phase `dirtydash-px3.1` establishes this glossary for the entire stream.

See also:

- [`/api/v1` Protocol And Privacy Invariants](./API_V1_INVARIANTS.md)
- [`ADR-0001: Hub/Collector Topology`](./adr/ADR-0001-hub-collector-topology.md)
- [`ADR-0002: Metadata-Only Privacy Boundary`](./adr/ADR-0002-metadata-only-privacy-boundary.md)
- [`ADR-0003: Tailscale And Fallback Administrator Authentication`](./adr/ADR-0003-tailscale-and-fallback-administrator-authentication.md)
- [`ADR-0004: SQLite Repository Seam`](./adr/ADR-0004-sqlite-repository-seam.md)

## Glossary

### Hub

The self-hosted Dirtydash node that owns the canonical fleet database, serves the dashboard, terminates authenticated `/api/v1` Collector traffic, and manages enrollment, credentials, backups, and administrative sessions. The Hub is distinct from loopback-only local `dirtydash serve`, although the Hub machine also runs a local Collector.

### Collector

The outbound-only per-machine Dirtydash runtime that scans local harness data, normalizes it to the accepted metadata contract, keeps a durable local manifest and outbox, and pushes idempotent batches to the Hub. A Collector is never an inbound service and never requires other machines to SSH into it.

### Machine

One enrolled host with a stable Machine ID and exactly one Collector instance for Dirtydash fleet purposes. A Machine may be the Hub host itself or any additional Linux/macOS host enrolled through Hub-side administration.

### Agent

A supported AI coding harness family represented in normalized usage data, such as Claude Code, Codex, OpenCode, Pi, or Hermes. Agents are discovered from usage; administrators enroll Machines, not Agents.

### Source Record

The metadata-only record a Collector derives from a local source artifact or source-root state so it can reconcile parsing work, explain provenance, and avoid duplicate ingestion. Collector-local manifests may retain machine-local file details, but any Hub-persisted source record stays within the privacy boundary and excludes raw session content and absolute paths.

### Usage Event

The smallest normalized unit of usage the fleet transports and persists. A Usage Event carries token, pricing, time, model, confidence, and provenance metadata; it contains no raw prompt, response, or copied session body. Its stable identity is Machine ID plus Agent plus Collector event fingerprint.

### Sync Run

One bounded Collector execution that scans sources, normalizes and queues Source Records and Usage Events, sends one or more `/api/v1` batches, and records acknowledgement/error state for diagnostics and reconciliation.

## Relationship Sketch

- A fleet has one canonical Hub and one or more Machines.
- Every Machine runs one Collector; the Hub machine runs a Collector too.
- Collectors produce Source Records and Usage Events during Sync Runs.
- The Hub persists accepted Usage Events and fleet metadata behind the repository seam.
- `/api/v1` carries only the metadata allowed by the privacy invariants.
