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
- The final PR #10 closeout correction is test-only: the migration regression uses distinct command, command-result, and acknowledgement sentinels, scans every Hub table for all of them, preserves the safe historical result, and reruns migration twice. Production migration behavior is unchanged.

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

Approved after four independent `thermo-nuclear-code-quality-review` rounds and three bounded repair passes.

- Initial review found the runtime was library-only, parser detection/privacy and identity were unsafe, pricing provenance was lost, credential/retry/command/update behavior was incomplete, and tests bypassed crash/watcher/long-poll states. Commit `ba27c4b` closed that set.
- Focused review then found repeated Refresh outbox growth and plaintext rotation secrets in Hub command persistence. Commit `eb8a614` implemented durable canonical event state and secret-free locally generated rotation with activation/proof.
- Migration review found legacy acknowledgement secrets were not scrubbed; `4d0456a` fixed production migration behavior.
- Final test-fixture review approved `08f9cd5`, confirming isolated sentinels, all-table scans, preserved safe history, and idempotent double migration.
- Final verdict: approved, no remaining blockers.

## CI And Gates

Owner: coordinator

State: `ci-unavailable-with-evidence`

Evidence:

- GitHub reported no configured check runs for PR #10.
- Coordinator reran `cargo fmt --all --check`: passed.
- Coordinator reran `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- Coordinator reran `cargo test --all-targets`: passed (54 unit tests, 6 CLI tests, 14 Collector integration tests).
- Coordinator reran `git diff --check origin/lavender/remote-hub-collector-fleet-implementation...HEAD`: passed.
- Focused migration, Hub security, replay, parser, watcher, rotation, and command tests passed during implementation and repair.

## PR And Commits

- Implementation commits: `cfe13e4` (`feat: add durable outbound collector runtime`) and `b87597d` (removed-source tombstones/manifest completeness).
- Repair commit: `eb8a614` (`repair collector refresh idempotency and secret-free rotation`).
- Final migration repair commit: `4d0456a` (`fix: scrub legacy collector command acknowledgements`).
- Test-only closeout correction: `08f9cd5` (`test: isolate legacy migration credential fixtures`).
- Phase PR: [#10](https://github.com/dirtydishes/dirtydash/pull/10), head `lavender/remote-hub-collector-fleet-3-collector`, base `lavender/remote-hub-collector-fleet-implementation`.
- Merged: 2026-07-15 at merge commit `68e4e55`.


## Beads Updates And Follow-Ups

- `dirtydash-px3.3` closed after acceptance, independent review, repair evidence, coordinator gates, and PR merge.
- Phase 4 (`dirtydash-px3.4`) is now ready.
- No follow-up issue was required; service installation and deployment wiring remain in the accepted Phase 4 scope.

## Plan Amendments

None.

## Context To Keep

Metadata redaction and stable event identity are acceptance boundaries, not later hardening.

## Closeout

Phase 3 complete. The outbound Collector runtime, five-Agent parsing, durable delivery, watcher/reconciliation fallback, command channel, privacy boundaries, and secret-free rotation are implemented; all independent review blockers were repaired; final review approved; coordinator gates passed; unavailable CI is documented; Beads is closed; and PR #10 is merged into the integration branch.
