# `/api/v1` Protocol And Privacy Invariants

Canonical scope: this document defines the network and persistence invariants for the Remote Hub and Collector Fleet stream.

See also:

- [`CONTEXT.md`](./CONTEXT.md)
- [`ADR-0001: Hub/Collector Topology`](./adr/ADR-0001-hub-collector-topology.md)
- [`ADR-0002: Metadata-Only Privacy Boundary`](./adr/ADR-0002-metadata-only-privacy-boundary.md)
- [`ADR-0003: Tailscale And Fallback Administrator Authentication`](./adr/ADR-0003-tailscale-and-fallback-administrator-authentication.md)
- [`ADR-0004: SQLite Repository Seam`](./adr/ADR-0004-sqlite-repository-seam.md)

## Scope

`/api/v1` is the versioned Hub/Collector boundary. It is the only accepted fleet ingestion protocol in this stream. Loopback-only local `dirtydash serve` remains a separate no-account experience.

## Protocol Invariants

- Every `/api/v1` request is authenticated as a specific enrolled Machine and Collector credential; administrator sessions never substitute for Collector credentials.
- Usage Event identity is stable across retries: `machine_id + agent + collector_event_fingerprint`.
- Collectors deliver at least once. The Hub must treat duplicates idempotently.
- Owner credential rotation commands carry only a non-secret rotation ID. The Collector generates the replacement secret, activates its hash through an authenticated overlap endpoint, proves the replacement, and commits locally only after Hub retirement.
- Batch acknowledgement happens only after the entire batch is durably committed.
- Incompatible protocol versions fail explicitly; silent downgrade is not an accepted behavior.
- Hub ingestion writes remain serialized behind the repository seam even when many Collectors are connected.
- Collectors keep unacknowledged work locally and reconcile on a periodic schedule in addition to best-effort watcher hints.

## Privacy Invariants

- `/api/v1` payloads are metadata-only.
- Allowed payloads include usage counts, timestamps, model/provider identifiers, confidence, pricing version, parser provenance, display-safe project/session/source identifiers, and sync diagnostics needed for correctness and troubleshooting.
- Forbidden payloads include raw prompts, raw responses, copied session files, absolute paths, SSH passwords, sudo passwords, and any other secret or content that would let the Hub reconstruct original session text.
- Collector-local manifests may retain machine-local file paths when needed for parsing, but Hub persistence stores only redacted or non-reversible identifiers.
- Deployment and enrollment secrets live only in process/request memory before they are discarded or transformed into hashed credentials.
- Hub command and acknowledgement persistence stores no raw Collector token; credential tables contain hashes only.

## Trust-Mode Invariants

- Tailscale Serve is the default private HTTPS entry point for Hub administration.
- Public reverse proxies ignore Tailscale identity headers and require fallback administrator authentication.
- Collector authentication, administrator authentication, and browser session security remain separate concerns.
- Browser-side administrative actions require normal session protections, including CSRF-aware state changes.

## Diagnostic Invariants

- Sync Run records must explain freshness, retry, and failure state without storing prohibited content.
- Privacy violations are correctness bugs, not optional redactions.
- New `/api/v1` endpoints or payload fields must link back to this document and the relevant ADRs before implementation proceeds.
