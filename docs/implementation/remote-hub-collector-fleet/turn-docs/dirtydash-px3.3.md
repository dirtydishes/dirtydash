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

Bound environment verification for this closeout: `/home/delta/dev/dirtydash-phase3` is the repository root and attached mutable worktree; symbolic branch is `lavender/remote-hub-collector-fleet-3-collector`; `origin` is `https://github.com/dirtydishes/dirtydash.git`; the branch targets the open PR #10 base `lavender/remote-hub-collector-fleet-implementation`. No Beads or merge operation was performed.

## Adaptations

- The phase PR targets the integration branch rather than `main`.
- The user-selected `dirtyloops.scout` override is `openai-codex/gpt-5.6-sol` with low reasoning and the original read-only tool boundary.
- Focused re-review returned the same mutable owner for two bounded repairs only: reconciliation event identity/outbox idempotency and secret-free command rotation.
- Final PR #10 review found one remaining narrow migration gap: legacy scrubbing covered `collector_command_results.result_json` but omitted `collector_commands.result_json`. The repair updates only credential/secret-bearing acknowledgement results to the existing redacted marker, preserves safe historical results, and remains idempotent on rerun.

## Discoveries And Decisions

- Accepted importer/Hermes scout `ce920025-7b9f-4210-9b66-66b2539d467f` identified the monolithic importer facade, extension-only detection, global parser version, path-dependent raw hash, and the absence of Hermes. Its accepted output was incorporated from the parent-mediated scout transcript (runtime artifact removed at closeout).
- Accepted runtime/outbox scout `f8288e54-8bf1-47bf-ac39-1a521fed9f50` confirmed the Phase 2 DTO/idempotency seams and required atomic manifest+canonical-outbox commit, matching-ack deletion, persisted retry state, command receipts, 20-second poll, and no Collector listener. Its accepted output was incorporated from the parent-mediated scout transcript (runtime artifact removed at closeout).
- `importers.rs` keeps its existing CLI facade while exposing a side-effect-free `ParserRegistry`/Collector parse boundary. Hermes supports aliases, default state/session/journal roots, format-evidence detection, a read-only `state.db` sessions parser, JSONL fallback, and a per-parser provenance version.
- Collector fingerprints exclude local paths, display salts, pricing, parser versions, and import time. The Collector builds separate typed redacted DTOs; `metadata_only` is forced true rather than copied from local import options. Complete reconciliation emits redacted tombstones for removed source files.
- `AppPaths` now derives a separate Collector SQLite path. Its manifest advance and immutable serialized `IngestBatchRequest` outbox append share one SQLite transaction. Outbox rows survive restart/offline states and are deleted only after an exact batch acknowledgement.
- Watch notifications are coalesced hints; startup, periodic fifteen-minute, manual, and watcher-fallback paths all use complete reconciliation. Failure is persisted and visible as degraded status. No inbound Collector port or deployment/fleet UI was added.
- Owner commands are typed and allowlisted for refresh, two-phase credential staging/commit, metadata-only diagnostics, and approved version/digest updates. Minimal authenticated Hub poll/ack and owner issue endpoints reuse the Phase 2 identity/ingestion contracts.
- The re-review found that forced Refresh reparsed correctly but compared `source:fingerprint` locally with `agent:event-fingerprint` in pending payloads and had no durable delivered-event state. `collector_event_manifests` now stores canonical identity, canonical payload fingerprint, emitted state, and delivery state in the same transaction as manifest/outbox advancement. Startup, manual, and owner Refresh paths therefore reparse when required while enqueueing only new or changed canonical events; missing timestamps use a stable fallback and tombstones remain manifest-driven.
- Credential rotation commands now carry only `rotation_id`. The Collector creates and durably stages its replacement token locally, activates only its SHA-256 hash through authenticated overlap endpoints, proves the replacement with the new bearer, then atomically commits the local credential. Hub rotation/command rows and acknowledgements reject or scrub raw credential-shaped values; old command rows are cleaned during migration, and old credentials remain valid until explicit proof.
- The final migration repair scrubs `collector_commands.result_json` using the same credential markers as legacy command-result cleanup (`credential_token`, `ddcol_`, and `secret`). Its regression inserts safe `command_json` plus a raw credential sentinel only in the acknowledgement result, runs migration twice, preserves a non-secret historical result, and scans every persistent SQLite table for the sentinel.

## Implementation And Delegation Evidence

The mutable implementation session incorporated both accepted read-only Sol-low scout outputs above. The implementation owner made all code/test changes; no scout edited repository files or Beads.

## Changed Behavior And Files

- `crates/dirtydash/src/collector.rs`: outbound Collector, redaction, manifests/outbox, retry policy/classification, watcher scheduling/degradation, command receipts/handlers, credential rotation, instance lease, and transport seam.
- `crates/dirtydash/src/importers.rs`: shared Collector parser registry, Hermes support/state DB fallback, format-evidence detection, parser versions, path-independent event fingerprints.
- `crates/dirtydash/src/db.rs`: Collector-only migration, event-manifest emitted/delivered state, identity/rotation, outbox/receipt/lock/command storage, and atomic reconciliation/delivery transactions.
- `crates/dirtydash/src/app_paths.rs`, `config.rs`, `cli.rs`, `lib.rs`: separate Collector path/config and `collector reconcile`/`collector diagnostics` CLI.
- `crates/dirtydash/src/hub/{mod.rs,repository.rs,router.rs,protocol.rs}`: non-secret rotation instruction, authenticated activation/proof endpoints, and command/ack secret rejection.
- `crates/dirtydash/tests/collector.rs`, `crates/dirtydash/tests/fixtures/{claude-code,codex,opencode,pi,hermes-agent}`: five-Agent real fixtures plus repeated startup/manual/Refresh, durable event-state, missing-timestamp, rotation fallback/retry/crash, and secret-free acknowledgement tests.
- `crates/dirtydash/src/hub/tests.rs`, `crates/dirtydash/tests/cli.rs`: command endpoint, overlap-proof, Hub-table secret scan, and Hermes test-environment isolation.

## Review

Focused independent re-review identified two confirmed blockers and no broader scope expansion. The earlier repair closed the canonical identity mismatch/durable delivery-state gap and removed raw credential material from command/ack persistence while preserving overlap fallback and crash replay. The final review found and repaired the acknowledgement-result migration omission described above. No merge was performed in this session.

## CI And Gates

Owner: current Phase 3 mutable owner; coordinator retains integration/merge ownership

State: passed locally; PR #10 push/remote verification passed

Evidence:

- The final red-capable migration regression initially failed with the sentinel still in `collector_commands.result_json`; it passes after the migration update.
- Focused migration tests pass (2 tests): `cargo test -p dirtydash --lib hub::tests::migration_ -- --nocapture`.
- Focused Hub security regression passes: `collector_rotation_uses_non_secret_instruction_and_secret_free_hub_persistence`.
- Focused Collector suite passes 14 tests, including repeated startup/manual/owner Refresh before and after delivery, missing timestamps, tombstones, terminal outbox bounding, local rotation generation, fallback, proof retry, restart reclaim, atomic commit, and acknowledgement redaction.
- Focused Hub suite passes 31 tests, including overlap activation/proof, idempotent retries, command JSON/ack secret rejection, all-table raw-token scan, and legacy command migration cleanup.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` passed.
- `cargo test --workspace --all-targets --all-features` passed: 54 unit tests, 6 CLI tests, and 14 Collector integration tests.
- `git diff --check` passed.
- Runtime-artifact check found no `.sqlite3`, `.db`, WAL/SHM, log, or temporary files outside ignored build/tracker directories.
- `git pull --rebase && git push` succeeded; the clean branch is up to date with `origin`, and PR #10 is open with the expected head/base refs.

## PR And Commits

- Implementation commits: `cfe13e4` (`feat: add durable outbound collector runtime`) and `b87597d` (removed-source tombstones/manifest completeness).
- Repair commit: `eb8a614` (`repair collector refresh idempotency and secret-free rotation`).
- Final migration repair commit: `4d0456a` (`fix: scrub legacy collector command acknowledgements`).
- Phase PR: [#10](https://github.com/dirtydishes/dirtydash/pull/10), head `lavender/remote-hub-collector-fleet-3-collector`, base `lavender/remote-hub-collector-fleet-implementation`, state open.
- Coordinator retains integration/merge ownership; this session does not mutate Beads or merge the PR.


## Beads Updates And Follow-Ups

Loop creation superseded old refresh, remote-sync, harness, and watcher issues with this phase where appropriate.

## Plan Amendments

None.

## Context To Keep

Metadata redaction and stable event identity are acceptance boundaries, not later hardening.

## Closeout

Final review and the bounded migration repair are complete in the Phase 3 mutable worktree. Code, regression coverage, focused security/migration checks, all requested Rust gates, runtime-artifact cleanup, and PR #10 push verification are recorded here. The coordinator retains Beads and merge ownership.
