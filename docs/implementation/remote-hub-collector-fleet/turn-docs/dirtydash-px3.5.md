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

### Tracer-bullet resume brief — 2026-07-15

```json
{
  "phase_issue_id": "dirtydash-px3.5",
  "active_slice": "dirtydash-px3.8",
  "risk": "high",
  "strategy": "sessions",
  "implementation_owner": "one durable Pi session bound to /home/delta/dev/dirtydash-phase5 on lavender/remote-hub-collector-fleet-5-fleet",
  "review_independence": "a fresh read-only thermo-nuclear review session after implementation ownership returns",
  "delegation_plan": [
    "complete only the active ready slice",
    "run focused and regression gates in the implementation session",
    "perform one bounded independent review and at most two convergent repair cycles before closeout",
    "return Beads, PR, merge, and phase advancement authority to the coordinator"
  ],
  "model_and_effort_rationale": "Enrollment crosses SSH deployment, transient credentials, authentication, ingest, command polling, and cleanup, so a strong model with high reasoning is proportionate.",
  "required_evidence": [
    "non-loopback canonical Hub URL",
    "unique Machine-bound credential",
    "production-path first ingest and command poll",
    "no secret in arguments, environment, logs, diagnostics, or persistence",
    "failed-attempt credential revocation and plaintext retry-state clearing"
  ],
  "user_constraints": [
    "continue autonomously across slices and phases",
    "structured completion callbacks without routine polling",
    "one active implementation PR and one mutable checkout owner",
    "two-cycle non-convergence guard"
  ]
}
```

The coordinator validated the Pi binding and certified adapter contract before resume, claimed `dirtydash-px3.8`, and transferred the clean phase checkout to the implementation session.

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
- Repair cycle 1 for PR #12 treats probe as a secret-bearing enrollment step, clears SSH/sudo inputs before request dispatch, and retries with an empty body rather than retaining plaintext in React closures.
- Repair cycle 2 branches before retry closure creation so secret-bearing enrollment retries bind only `step(path, {})` and never lexically reference the original request object.
- Repair cycle 1 scopes long-poll wakeups by Machine ID so a command for one Collector cannot be consumed by another Machine's waiter.
- Repair cycle 1 strengthens the hosted enrollment-to-Collector evidence through the DeploymentRunner install seam, generated runtime config, restrictive secret store, production `Config::load`/`Collector::open`, real HTTP ingest, and command polling. The exact `/execute` handler plus non-loopback HTTPS/SSH/service-manager path remains an external integration gate.
- `.pi-subagents/` was removed before closeout; no mutable harness/session artifacts are part of the worktree.

## Implementation And Delegation Evidence

The bound implementation checkout contains the Hub fleet repository/router, additive schema migration, Collector repair command, hosted enrollment endpoints, signed rollout persistence/coordinator, and Machines workspace. This repair pass adds typed bounded command/receipt schemas, deterministic update and rollback commands, transactional lifecycle revisions, rollback desired-version/runtime state, canonical hosted Hub URL rendering, request-scoped credential reservation and atomic restrictive secret transfer, server-owned restart reconciliation, timeout recovery, cleanup retry, private snapshot permissions, and fail-closed private Tailscale identity handling. The dashboard uses native Tab order, tablist arrow/Home/End navigation, explicit icon-plus-text states, focus-visible styling, reduced-motion support, container-responsive controls, a body-portal destructive dialog with complete background inertness, mutation/load error separation, secret-free retry closures, and server-owned receipt rendering. Contract and rendered axe/focus tests cover the repaired surface.

Slice `dirtydash-px3.8` tightens hosted enrollment through first Hub ingest: hosted execute now rejects loopback/unspecified canonical Hub origins, revokes the reserved pending Collector credential on post-reservation failure paths, and leaves retry bound to the same enrollment credential row with a newly hashed secret. Repair cycle 1 updates the production HTTP Collector test to install the Hub-issued credential through `DeploymentRunner`, verify the generated config omits raw credential material, verify `secrets.json` is restrictive secret material, load that installed config/secret through `Config::load` and `Collector::open`, then run real HTTP ingest and diagnostics polling against an Axum Hub listener.

## Changed Behavior And Files

- Backend: `crates/dirtydash/src/hub/fleet.rs`, `hub/router.rs`, `hub/repository.rs`, `hub/auth.rs`, `hub/mod.rs`, `hub/protocol.rs`, `db.rs`, `config.rs`, `enrollment.rs`, and `collector.rs`.
- Frontend: `dashboard/src/machines.tsx`, `dashboard/src/main.tsx`, `dashboard/src/styles.css`, `dashboard/tests/machines-contract.test.mjs`, and `dashboard/tests/machines-a11y.test.tsx`.
- Dashboard tooling: `dashboard/package.json`, `dashboard/package-lock.json`, and `dashboard/vite.config.ts` add strict TypeScript types plus rendered Vitest/jsdom/axe coverage.
- Generated dashboard artifacts: `dashboard/dist` is regenerated from the repaired portal/inert and secret-state UI; stale hashed assets are removed.
- Documentation: this turn document, the phase docs, and `/api/v1` invariant notes record canonical URLs, secret transfer, receipt/rollback, lifecycle, and snapshot-permission contracts.
- Slice `dirtydash-px3.8`: `crates/dirtydash/src/hub/router.rs`, `hub/repository.rs`, `hub/mod.rs`, and `hub/tests.rs` cover non-loopback hosted canonical URL enforcement, pending credential revocation/retry semantics, first-ingest/command-poll production HTTP evidence, and Machine-scoped long-poll notification.
- Repair cycle 1: `dashboard/src/machines.tsx`, `dashboard/tests/machines-a11y.test.tsx`, and `dashboard/tests/machines-contract.test.mjs` cover probe secret clearing and retry-body scrubbing.
- Repair cycle 2: `dashboard/src/machines.tsx`, `dashboard/tests/machines-a11y.test.tsx`, `dashboard/tests/machines-contract.test.mjs`, and regenerated `dashboard/dist` assets cover secret-bearing retry closures with no lexical request-body capture.

## Review

This bounded repair pass addresses the independent PR #12 security/correctness and accessibility findings in the same implementation checkout. Repair cycle 1 addresses the follow-up review findings for probe plaintext retention, production-path evidence, and wrong-Collector long-poll wakeups without widening the phase scope. Repair cycle 2 addresses the remaining high finding by ensuring a secret-bearing rendered retry handler's closure source contains `step(path, {})` and no `body` or `carriesSecret` reference. A fresh external browser/tailnet review remains an integration gate; local rendered modal/axe coverage and typed backend tests provide the available evidence.

## CI And Gates

Owner: implementation session

State: local gates passed; PR #12 is open and no remote checks are reported yet

Evidence:

- `cargo fmt --all -- --check` passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo test --all-targets --all-features` passed: 117 library tests, 9 CLI tests, and 15 Collector integration tests.
- `npm --prefix dashboard run build` passed with regenerated hashed `dashboard/dist` assets.
- `npm --prefix dashboard run test` passed: 2 rendered modal focus/inert/axe tests.
- `npm --prefix dashboard run test:contract` passed: `Machines DOM/a11y contract: passed`.
- `npm --prefix dashboard exec tsc -- --noEmit` passed with dashboard-local React typings.
- Production bundle inspection confirmed the generated JavaScript contains the portal/inert modal behavior.
- `git diff --check` passed.

Additional `dirtydash-px3.8` evidence:

- `cargo test -p dirtydash hosted_enrollment_canonical_url_and_pending_credentials_are_safe_to_retry -- --nocapture` passed.
- `cargo test -p dirtydash enrolled_collector_http_path_ingests_usage_and_polls_commands -- --nocapture` passed.
- `cargo test -p dirtydash hub::tests:: -- --nocapture` passed: 41 Hub tests.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo test --all-targets --all-features` passed: 119 library tests, 9 CLI tests, and 15 Collector integration tests.
- `npm --prefix dashboard run test:contract` passed.
- `npm --prefix dashboard run test` passed: 2 rendered tests.
- `cd dashboard && npx tsc --noEmit` passed.
- `cd dashboard && npm run build` passed.

Repair cycle 1 evidence:

- `cargo test -p dirtydash enrolled_collector_uses_installed_config_secret_for_http_ingest_and_commands -- --nocapture` passed.
- `cargo test -p dirtydash collector_long_poll_wakes_target_machine_under_concurrent_polls -- --nocapture` passed.
- `cargo test -p dirtydash hub::tests:: -- --nocapture` passed: 42 Hub tests.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo test --all-targets --all-features` passed: 120 library tests, 9 CLI tests, and 15 Collector integration tests.
- `npm --prefix dashboard run test` passed: 3 rendered tests including probe secret retry coverage.
- `npm --prefix dashboard run test:contract` passed: `Machines DOM/a11y contract: passed`.
- `cd dashboard && npx tsc --noEmit` passed.
- `npm --prefix dashboard run build` passed with regenerated `dashboard/dist/assets/index-hzSTiPxX.js`.
- `git diff --check` passed.

Repair cycle 2 evidence:

- `npm --prefix dashboard run test` passed: 3 rendered tests; the enrollment retry regression inspects the rendered retry button handler and rejects `body`/`carriesSecret` lexical references.
- `npm --prefix dashboard run test:contract` passed: source contract rejects the old conditional retry closure and requires the secret branch to bind `step(path, {})` without `body`.
- `cd dashboard && npx tsc --noEmit` passed.
- `npm --prefix dashboard run build` passed with regenerated `dashboard/dist/assets/index-B9mQIdSB.js`.
- `git diff --check` passed.
- No Rust files changed; no Rust regression was required for this dashboard-only repair.

## PR And Commits

Base implementation commit `d2ace45` remains the PR #12 base. The bounded repair is kept on `lavender/remote-hub-collector-fleet-5-fleet`; commit and push evidence are recorded only after the final local gates pass. The PR target remains `lavender/remote-hub-collector-fleet-implementation`.

## Beads Updates And Follow-Ups

Beads was not mutated in this child session. The parent coordinator retains issue status, review callbacks, integration merge, and any follow-up issue filing.

## Plan Amendments

The implementation added a dedicated diagnostics action endpoint and an explicit signed-artifact coordinator entry point accepting only Deployment-verified artifacts; test-only evidence remains confined to Rust test builds.

## Context To Keep

Archive and permanent deletion are deliberately separate operations. Hosted signed enrollment/update operations fail closed unless the Hub has the configured publisher public key, key ID, and fingerprint.

## Closeout

The long-running Pi coordinator was stopped at a file-stable boundary after the repair owner continued broadening scope beyond repeated green gates and the compacted parent session no longer exposed its steering tool. The preserved checkout passed the full external checkpoint gates after two repair-caused fixture/module-placement corrections. `.pi-subagents/` is absent, generated dashboard artifacts are refreshed, and external CI, browser, and real-tailnet checks remain integration-owned. Phase 5 stays open and PR #12 stays unmerged for the approved tracer-bullet closure plan.
