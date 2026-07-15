# Phase 3 Turn Doc: Collector Runtime

Beads issue: `dirtydash-px3.3`

Phase doc: `docs/implementation/remote-hub-collector-fleet/03-collector-runtime.md`

## Accepted Outcome

Outbound-only Collectors parse locally and deliver metadata reliably through durable outboxes and reconciliation.

## Orchestration Brief

```json
{
  "phase_issue_id": "dirtydash-px3.3",
  "risk": "high",
  "strategy": "sessions",
  "implementation_owner": "one durable Luna-max Pi implementation session bound to lavender/remote-hub-collector-fleet-3-collector",
  "review_independence": "a separate fresh Luna-max Pi review session using thermo-nuclear-code-quality-review after implementation ownership returns",
  "delegation_plan": [
    "Sol-low read-only pi-subagents scouts inventory importer/Hermes seams and durable runtime/outbox/watch/retry boundaries for the implementation session",
    "the durable implementation session owns all code/test mutations, integration, commits, push, and one phase PR",
    "fresh review receives separate Sol-low privacy and reliability scout evidence; bounded repairs return to a single implementation owner"
  ],
  "model_and_effort_rationale": "Collector privacy, at-least-once delivery, parser compatibility, and long-running retry/reconciliation behavior justify Luna max for mutable and independent-review sessions; bounded repository scouts use Sol low.",
  "required_evidence": [
    "real fixtures for Claude Code, Codex, OpenCode, Pi, and Hermes",
    "manifest/parser-upgrade and malformed-record tests",
    "offline durable outbox replay with deduplicated Hub arrival",
    "payload assertions excluding raw content and absolute paths",
    "watcher debounce/failure fallback to fifteen-minute and manual reconciliation",
    "retry backoff and twenty-second command long-poll tests",
    "cargo fmt, clippy, cargo test, independent review, and terminal CI state"
  ],
  "user_constraints": [
    "orchestrator launches separate implementation and review sessions",
    "pi-subagents support those sessions as read-only Sol-low scouts",
    "merge the phase PR into lavender/remote-hub-collector-fleet-implementation before advancing"
  ]
}
```

The implementation session is the sole mutable owner of the phase worktree. The coordinator owns Beads, integration, CI, and callbacks; scouts remain read-only.

## Adaptations

- The phase PR targets the integration branch rather than `main`.
- The user-selected `dirtyloops.scout` override is `openai-codex/gpt-5.6-sol` with low reasoning and the original read-only tool boundary.

## Discoveries And Decisions

- Accepted importer/Hermes scout `ce920025-7b9f-4210-9b66-66b2539d467f` identified the monolithic importer facade, extension-only detection, global parser version, path-dependent raw hash, and the absence of Hermes. Its accepted output was incorporated from the parent-mediated scout transcript (runtime artifact removed at closeout).
- Accepted runtime/outbox scout `f8288e54-8bf1-47bf-ac39-1a521fed9f50` confirmed the Phase 2 DTO/idempotency seams and required atomic manifest+canonical-outbox commit, matching-ack deletion, persisted retry state, command receipts, 20-second poll, and no Collector listener. Its accepted output was incorporated from the parent-mediated scout transcript (runtime artifact removed at closeout).
- `importers.rs` keeps its existing CLI facade while exposing a side-effect-free `ParserRegistry`/Collector parse boundary. Hermes supports aliases, default state/session/journal roots, format-evidence detection, a read-only `state.db` sessions parser, JSONL fallback, and a per-parser provenance version.
- Collector fingerprints exclude local paths, display salts, pricing, parser versions, and import time. The Collector builds separate typed redacted DTOs; `metadata_only` is forced true rather than copied from local import options.
- `AppPaths` now derives a separate Collector SQLite path. Its manifest advance and immutable serialized `IngestBatchRequest` outbox append share one SQLite transaction. Outbox rows survive restart/offline states and are deleted only after an exact batch acknowledgement.
- Watch notifications are coalesced hints; startup, periodic fifteen-minute, manual, and watcher-fallback paths all use complete reconciliation. Failure is persisted and visible as degraded status. No inbound Collector port or deployment/fleet UI was added.
- Owner commands are typed and allowlisted for refresh, two-phase credential staging/commit, metadata-only diagnostics, and approved version/digest updates. Minimal authenticated Hub poll/ack and owner issue endpoints reuse the Phase 2 identity/ingestion contracts.

## Implementation And Delegation Evidence

The mutable implementation session incorporated both accepted read-only Sol-low scout outputs above. The implementation owner made all code/test changes; no scout edited repository files or Beads.

## Changed Behavior And Files

- `crates/dirtydash/src/collector.rs`: outbound Collector, redaction, manifests/outbox, retry policy/classification, watcher scheduling/degradation, command receipts/handlers, credential rotation, instance lease, and transport seam.
- `crates/dirtydash/src/importers.rs`: shared Collector parser registry, Hermes support/state DB fallback, format-evidence detection, parser versions, path-independent event fingerprints.
- `crates/dirtydash/src/db.rs`: Collector-only migration, identity/rotation, manifest/outbox/receipt/lock/command storage, atomic reconciliation transaction.
- `crates/dirtydash/src/app_paths.rs`, `config.rs`, `cli.rs`, `lib.rs`: separate Collector path/config and `collector reconcile`/`collector diagnostics` CLI.
- `crates/dirtydash/src/hub/{mod.rs,repository.rs,router.rs}`: public protocol command types and minimal authenticated command issue/poll/ack endpoints.
- `crates/dirtydash/tests/collector.rs`, `crates/dirtydash/tests/fixtures/{claude-code,codex,opencode,pi,hermes-agent}`: five-Agent real fixtures and focused malformed/token/confidence/redaction/replay/restart/parser-upgrade/watcher/command/backoff/single-instance/state-db tests.
- `crates/dirtydash/src/hub/tests.rs`, `crates/dirtydash/tests/cli.rs`: command endpoint coverage and Hermes test-environment isolation.

## Review

Coordinator-owned independent review remains pending. No review or merge was performed in this session.

## CI And Gates

Owner: coordinator

State: local implementation gates passed; independent review/CI pending

Evidence:

- `cargo fmt --all -- --check` passed after formatting.
- `cargo clippy --all-targets -- -D warnings` passed.
- `cargo test --all-targets` passed: 47 unit tests, 6 CLI tests, and 6 Collector integration tests.
- `git diff --check` passed.

## PR And Commits

Implementation commit/PR and integration push remain coordinator-owned closeout actions.


## Beads Updates And Follow-Ups

Loop creation superseded old refresh, remote-sync, harness, and watcher issues with this phase where appropriate.

## Plan Amendments

None.

## Context To Keep

Metadata redaction and stable event identity are acceptance boundaries, not later hardening.

## Closeout

Not started.
