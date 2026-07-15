# Phase 2 Turn Doc: Storage and Protocol Foundation

Beads issue: `dirtydash-px3.2`

Phase doc: `docs/implementation/remote-hub-collector-fleet/02-storage-and-protocol-foundation.md`

## Accepted Outcome

Authenticated `/api/v1` ingestion and the SQLite repository foundation safely accept fleet metadata.

## Orchestration Brief

```json
{
  "phase_issue_id": "dirtydash-px3.2",
  "risk": "high",
  "strategy": "sessions",
  "implementation_owner": "one durable Pi implementation session bound to the phase-2 worktree and symbolic branch lavender/remote-hub-collector-fleet-2-foundation",
  "review_independence": "a separate fresh Pi review session using thermo-nuclear-code-quality-review after implementation ownership returns",
  "delegation_plan": [
    "read-only pi-subagents scouts inventory storage/migration seams, HTTP/auth trust boundaries, and time-zone/idempotency test surfaces for the implementation session",
    "the durable implementation session synthesizes scout evidence, owns all code/test mutations, commits, pushes, and opens one phase PR",
    "the fresh review session receives separate read-only pi-subagents security and correctness evidence; it may repair bounded findings when authorized"
  ],
  "model_and_effort_rationale": "Use a strong coding model with high reasoning for the implementation and independent review because this phase establishes security, storage, identity, and transactional protocol contracts; use smaller bounded models for read-only inventories.",
  "required_evidence": [
    "repository seam and WAL migration tests",
    "transactional idempotent batch tests including duplicate, retry, partial failure, and concurrency cases",
    "credential rotation/revocation and Argon2id administrator-session tests",
    "forged Tailscale-header isolation and CSRF tests",
    "UTC storage and owner-time-zone/DST rebucketing tests",
    "cargo test",
    "independent review and terminal CI state"
  ],
  "user_constraints": [
    "orchestrator launches separate implementation and review sessions",
    "pi-subagents support those sessions as read-only scouts",
    "merge the phase PR into lavender/remote-hub-collector-fleet-implementation before advancing"
  ]
}
```

The implementation session is the sole mutable owner of the phase worktree. The coordinator owns Beads, integration, CI resolution, and callbacks; pi-subagents scouts remain read-only.

## Adaptations

- The phase PR targets the user-requested integration branch rather than `main`.
- Supporting pi-subagents are parent-mediated read-only scouts because the certified backend does not grant them write ownership or broad-review closeout authority.

## Discoveries And Decisions

- Incorporated the storage/migration scout run `d5d62b5f-e2aa-4776-a957-440a07be566f` before broad implementation. Its key findings were adopted directly: keep WAL+FK setup at the SQLite connection seam, preserve additive migration compatibility for existing `usage_events`, keep `raw_event_hash` compatibility, replace implicit per-write behavior with a repository-owned transaction for batch ingestion, and stop relying on SQLite `date(...)` for owner-facing time-zone aggregation.
- Incorporated the API/auth trust-boundary scout run `93cf55da-21e5-40bb-9250-68a58c464cb3`. The implementation keeps loopback `dirtydash serve` unchanged and isolates Hub `/api/v1` + admin/session logic in a separate `hub` module/router with explicit `PrivateTailscale` vs `Public` listener trust modes. Public mode ignores forged Tailscale headers.
- Incorporated the accepted Sol-low protocol scout run `74805b43-e19c-4024-ac22-55702ea13d7c`. The finished slice wires typed fleet identity into behavior, validates metadata-only DTOs before persistence, makes request-fingerprint idempotency/conflict behavior atomic inside one repository-owned transaction, explicitly rejects unsupported protocol versions, and rebuckets owner time-zone queries in Rust across midnight, DST gaps, and DST folds.
- The narrow repository seam for new Hub behavior is `HubRepository` in `crates/dirtydash/src/hub.rs`. It owns owner auth/session lifecycle, collector credential lifecycle, authenticated `/api/v1` ingestion, transaction boundaries, and owner time-zone aggregation while reusing the existing SQLite file and legacy read-path compatibility.
- Legacy read APIs remain intact for this release. The existing loopback server and CLI serve path still target the unauthenticated local dashboard contract while the new Hub foundation stays adjacent to it.

## Implementation And Delegation Evidence

- Added a new `hub` module containing:
  - `HubRepository` with serialized write ownership over batch/event/checkpoint/manifest/sync-run commits.
  - versioned `/api/v1` admin and collector routers.
  - Argon2id owner bootstrap/login, cookie sessions, CSRF enforcement, and Tailscale/public trust separation.
  - collector bearer authentication with hashed credential rotation/revocation.
  - Rust-side owner time-zone day bucketing using IANA zones instead of SQLite `date(...)`.
- Extended the shared SQLite schema in `db.rs` with additive guarded migrations for fleet foundation tables and compatibility columns on existing `usage_events` rows.
- Preserved accepted compatibility constraints by backfilling `machine_id`, `agent`, and `collector_event_fingerprint` from old columns/identities and keeping existing local read/import paths intact.

## Changed Behavior And Files

- Added Hub foundation module:
  - `crates/dirtydash/src/hub.rs`
- Extended crate surface:
  - `crates/dirtydash/src/lib.rs`
- Added dependencies for Argon2id auth, time-zone rebucketing, and router testing:
  - `crates/dirtydash/Cargo.toml`
  - `Cargo.lock`
- Expanded SQLite compatibility + fleet schema migration coverage:
  - `crates/dirtydash/src/db.rs`
- Updated phase execution record with implementation evidence:
  - `docs/implementation/remote-hub-collector-fleet/turn-docs/dirtydash-px3.2.md`

## Review

Pending independent review session.

## CI And Gates

Owner: implementation session

State: local gates passing; independent review and terminal CI still pending coordinator handoff

Evidence:

- `cargo test hub --no-run`
- `cargo test hub -- --nocapture`
- `cargo test`

## PR And Commits

- Commits:
  - `b071bac` — `Add hub protocol and auth foundation`
  - `3655174` — `Record phase 2 implementation evidence`
- PR: #9 — `Phase 2: add hub protocol and auth foundation`
- Branch: `lavender/remote-hub-collector-fleet-2-foundation`
- Target: `lavender/remote-hub-collector-fleet-implementation`

## Beads Updates And Follow-Ups

Loop creation established the issue and dependency graph.

## Plan Amendments

None.

## Context To Keep

Phase 1 must establish the canonical domain and ADRs first.

## Closeout

Implementation ownership complete for this pass. Local gates pass, the phase branch is pushed, and PR #9 is open for the coordinator's independent review/CI path.
