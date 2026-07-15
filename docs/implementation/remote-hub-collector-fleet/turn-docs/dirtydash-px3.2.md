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
- The narrow repository seam for new Hub behavior is `HubRepository` in `crates/dirtydash/src/hub/repository.rs` plus the auth/session implementation in `crates/dirtydash/src/hub/auth.rs`. It owns owner auth/session lifecycle, collector credential lifecycle, authenticated `/api/v1` ingestion, transaction boundaries, and owner time-zone aggregation while reusing the existing SQLite file and legacy read-path compatibility.
- Thermo-nuclear review findings were accepted as bounded Phase 2 repairs: `first_owner` Tailscale login was an authorization bypass; direct/proxy header provenance and fresh public bootstrap were under-specified; cookie flags were implicit; the original failure/concurrency tests did not exercise the dangerous seams; display-safe fields admitted arbitrary content; additive migrations were not serialized; and API errors exposed raw internal text.
- The repair design persists exact owner-to-Tailscale identities in `owner_tailscale_identities`, supports explicit configured mappings through `HubRouterConfig`/`Config::hub`, separates `PrivateTailscale`, `TrustedProxy`, `Public`, and explicit `LoopbackHttp` boundaries, and makes public bootstrap disabled unless a loopback or setup-token boundary is explicitly selected.
- Legacy read APIs remain intact for this release. The existing loopback server and CLI serve path still target the unauthenticated local dashboard contract while the new Hub foundation stays adjacent to it.

## Implementation And Delegation Evidence

- Split the former roughly 2,439-line Hub file into cohesive modules: `hub/protocol.rs` (DTO validation and timestamp normalization), `hub/auth.rs` (owner/session/credential auth), `hub/repository.rs` plus `hub/ingestion.rs` (repository and transactional writes), `hub/router.rs` (narrow HTTP/config boundary), `hub/errors.rs`, and `hub/tests.rs`.
- Replaced implicit Tailscale owner selection with persisted/configured exact identity mappings, explicit trusted-proxy provenance markers, mismatch rejection, forged-direct negatives, loopback/setup-token-only fresh bootstrap, and fail-closed generic API errors.
- Added explicit cookie transport configuration. Secure cookies are the default and are forced for Tailscale, HTTPS/public, and trusted-proxy modes; only `ListenerTrustMode::LoopbackHttp` can select insecure loopback cookies. Login/bootstrap and logout behavior are covered.
- Added a final-insert SQLite failure injection test that checks rollback of sync runs, manifests, checkpoints, usage events, and ingest batches; raced the same batch through independently constructed repositories; persisted non-UTC RFC3339 normalization; and added DST transition/local-midnight aggregation cases.
- Tightened display-safe identifiers/checkpoints to bounded ASCII, no whitespace/control text, and no absolute paths. Additive SQLite migrations now run under `BEGIN IMMEDIATE` and commit as one unit.
- Extended the shared SQLite schema in `db.rs` with additive guarded migrations for fleet foundation tables, compatibility columns, and owner identity mappings.
- Preserved accepted compatibility constraints by backfilling `machine_id`, `agent`, and `collector_event_fingerprint` from old columns/identities and keeping existing local read/import paths intact.

## Changed Behavior And Files

- Replaced `crates/dirtydash/src/hub.rs` with the split Hub module set under `crates/dirtydash/src/hub/`: `mod.rs`, `protocol.rs`, `auth.rs`, `repository.rs`, `ingestion.rs`, `router.rs`, `errors.rs`, and `tests.rs`.
- Added persisted/configured Hub auth and transport settings:
  - `crates/dirtydash/src/config.rs`
  - `crates/dirtydash/src/db.rs`
- Preserved the crate module surface in `crates/dirtydash/src/lib.rs`; no dependency change was needed for the repair.
- Kept rustfmt clean-up required by `cargo fmt --check` in `crates/dirtydash/src/importers.rs`.
- Updated this phase execution record with repair evidence.

## Review

Thermo-nuclear review findings were repaired in this bounded pass. The coordinator still owns final independent review, CI interpretation, Beads updates, and merge. No Beads state was mutated by this repair owner.

Repair evidence includes approved persisted/configured Tailscale mappings, mismatch and forged-direct negatives, trusted-proxy provenance negatives, secure/insecure cookie transport tests, explicit setup-only bootstrap tests, final-insert rollback coverage, independent-repository same-batch races, prompt-like display/checkpoint rejection, migration serialization, generic internal API errors, non-UTC persistence, and DST/local-midnight rebucketing.

## CI And Gates

Owner: repair implementation session

State: local gates passing; coordinator terminal review/CI remains pending

Evidence:

- `cargo test -p dirtydash --lib hub::tests`
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

## PR And Commits

- Commits:
  - `b071bac` — `Add hub protocol and auth foundation`
  - `3655174` — `Record phase 2 implementation evidence`
  - `a558338` — `Repair phase 2 hub security and transaction seams`
- PR: #9 — `Phase 2: add hub protocol and auth foundation`
- Branch: `lavender/remote-hub-collector-fleet-2-foundation`
- Target: `lavender/remote-hub-collector-fleet-implementation`

## Beads Updates And Follow-Ups

No Beads mutation was performed by this repair owner. Coordinator retains issue status, follow-ups, final review, CI, and merge decisions.

## Plan Amendments

None.

## Context To Keep

Phase 1 must establish the canonical domain and ADRs first.

## Closeout

Repair implementation and local gates are complete. Commit `a558338` contains the repairs; the branch remains open for coordinator-owned final review/CI/Beads/merge.
