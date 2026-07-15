# Phase 5 Turn Doc: Fleet Management

Beads issue: `dirtydash-px3.5`

Phase doc: `docs/implementation/remote-hub-collector-fleet/05-fleet-management.md`

## Accepted Outcome

Owners can safely understand and operate Machine lifecycle, health, credentials, repair, and rolling updates.

## Orchestration Brief

```json
{
  "phase_issue_id": "dirtydash-px3.5",
  "risk": "high",
  "strategy": "sessions",
  "implementation_owner": "one durable Luna-max Pi implementation session bound to lavender/remote-hub-collector-fleet-5-fleet",
  "review_independence": "a separate fresh Luna-max Pi thermo-nuclear review session after implementation ownership returns",
  "delegation_plan": [
    "Sol-low read-only pi-subagents scouts inventory fleet lifecycle/update backend seams and existing dashboard interaction/design-system seams",
    "the durable implementation session applies frontend-design and impeccable product-register guidance and owns all backend/frontend/test mutations plus one PR",
    "fresh review receives separate Sol-low security/correctness and accessibility/UX evidence; bounded fixes return to one mutable owner"
  ],
  "model_and_effort_rationale": "Machine lifecycle/destructive semantics, rolling updates, credential controls, and a dense accessible administrative UI justify Luna max for implementation/review; bounded scouts use Sol low.",
  "required_evidence": [
    "hosted UI completes the Phase 4 Hub-side SSH enrollment flow",
    "explicit text+icon Machine states and compatibility status",
    "distinct refresh, rotate, repair, archive, typed-confirm deletion semantics",
    "snapshot-first Hub update then per-Collector update with independent rollback",
    "current+previous protocol compatibility tests",
    "keyboard, responsive desktop/tablet, reduced-motion, contrast and non-color status evidence",
    "cargo and dashboard gates, independent review, terminal CI state"
  ],
  "user_constraints": [
    "separate implementation and review sessions",
    "parent-mediated Sol-low pi-subagents scouts",
    "merge the phase PR into lavender/remote-hub-collector-fleet-implementation before advancing"
  ]
}
```

The implementation session is sole mutable owner. The coordinator owns Beads, integration, CI, and callbacks. The Machines UI must preserve Dirtydash's calm, dense, terminal-native product register rather than introducing generic SaaS cards.

## Adaptations

- The phase PR targets the integration branch rather than `main`.
- The bounded PR #12 repair pass keeps administrative actions available at compact widths. Container-responsive layout messaging and wrapping replace viewport-width authorization or hidden controls.

## Discoveries And Decisions

- Machine state is derived from observation timestamps, pending commands, diagnostics, credentials, protocol compatibility, and desired/current Collector versions; it is not an opaque persisted health enum.
- Archive/remove revokes credentials while retaining the Machine root and history. Permanent deletion is separate, requires exact `DELETE <display_name>` confirmation plus revision/name checks, and cascades inside one transaction.
- Hosted enrollment reuses the Phase 4 `EnrollmentWorkflow`, `SshEnrollmentBackend`, managed known-hosts, and `DeploymentRunner` seams. Drafts persist sanitized state only; secrets remain request-scoped.
- Fleet updates require an anchored Ed25519 signed manifest, persist Hub snapshot/update/health before Collector nodes, and record independent node receipts/rollback states. Only current and previous Collector protocols are accepted.
- The PR #12 repair pass makes command/result and update-receipt payloads typed and bounded, binds acknowledgements to issued command variants, and accepts completion only from an authenticated Collector after a new runtime generation proves restart and health.
- Hub restart reconciliation remains resumable across the old/new process boundary; the browser can request execution/reconciliation but cannot submit health, signature, or receipt evidence.
- `.pi-subagents/` was removed before closeout; no mutable harness/session artifacts are part of the worktree.

## Implementation And Delegation Evidence

The bound implementation checkout contains the Hub fleet repository/router, additive schema migration, Collector repair command, hosted enrollment endpoints, signed rollout persistence/coordinator, and Machines workspace. The repair pass adds typed bounded command/receipt schemas, deterministic update commands, transactional lifecycle revisions, rollback desired-version/runtime state, server-owned restart reconciliation, cleanup retry, and fail-closed private Tailscale identity handling. The dashboard uses native Tab order, tablist arrow/Home/End navigation, explicit icon-plus-text states, focus-visible styling, reduced-motion support, container-responsive controls, modal focus/inert behavior, mutation/load error separation, and server-owned receipt rendering. Contract and rendered axe/focus tests cover the repaired surface.

## Changed Behavior And Files

- Backend: `crates/dirtydash/src/hub/fleet.rs`, `hub/router.rs`, `hub/repository.rs`, `hub/auth.rs`, `hub/mod.rs`, `hub/protocol.rs`, `db.rs`, `config.rs`, `enrollment.rs`, and `collector.rs`.
- Frontend: `dashboard/src/machines.tsx`, `dashboard/src/main.tsx`, `dashboard/src/styles.css`, `dashboard/tests/machines-contract.test.mjs`, and `dashboard/tests/machines-a11y.test.tsx`.
- Dashboard tooling: `dashboard/package.json`, `dashboard/package-lock.json`, and `dashboard/vite.config.ts` add strict TypeScript types plus rendered Vitest/jsdom/axe coverage.
- Documentation: this turn document and the existing phase loop-state handoff. Protected committed `dashboard/dist` artifacts were not regenerated into the repair diff.

## Review

This bounded repair pass addresses the independent PR #12 security/correctness and accessibility findings in the same implementation checkout. A fresh external browser/tailnet review remains an integration gate; local rendered modal/axe coverage and typed backend tests provide the available evidence.

## CI And Gates

Owner: implementation session

State: local gates passed; PR #12 is open and no remote checks are reported yet

Evidence:

- `cargo fmt --all -- --check` passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo test --all-targets --all-features` passed: 115 unit/integration tests plus collector/CLI suites.
- `npm --prefix dashboard run build` passed.
- `npm --prefix dashboard run test` passed: 2 rendered modal focus/inert/axe tests.
- `npm --prefix dashboard run test:contract` passed: `Machines DOM/a11y contract: passed`.
- `npm --prefix dashboard exec tsc -- --noEmit` passed with dashboard-local React typings.
- `git diff --check` passed.

## PR And Commits

Base implementation commit `d2ace45` remains the PR #12 base. The bounded repair commit is recorded after local validation; PR #12 continues to target `lavender/remote-hub-collector-fleet-implementation`.

## Beads Updates And Follow-Ups

Beads was not mutated in this child session. The parent coordinator retains issue status, review callbacks, integration merge, and any follow-up issue filing.

## Plan Amendments

The implementation added a dedicated diagnostics action endpoint and an explicit signed-artifact coordinator entry point accepting only Deployment-verified artifacts; test-only evidence remains confined to Rust test builds.

## Context To Keep

Archive and permanent deletion are deliberately separate operations. Hosted signed enrollment/update operations fail closed unless the Hub has the configured publisher public key, key ID, and fingerprint.

## Closeout

The bounded repair implementation and local validation are complete. `.pi-subagents/` is absent, protected generated dashboard artifacts are unchanged, and the repair commit/push evidence is recorded above. External CI, browser, and real-tailnet checks remain integration-owned.
