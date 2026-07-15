# Phase 1 Turn Doc: Documentation and Tracker Reset

Beads issue: `dirtydash-px3.1`

Phase doc: `docs/implementation/remote-hub-collector-fleet/01-documentation-and-tracker-reset.md`

## Accepted Outcome

Repository and tracker language consistently express the accepted Hub/Collector architecture and boundaries.

## Orchestration Brief

```json
{
  "phase_issue_id": "dirtydash-px3.1",
  "risk": "medium",
  "strategy": "sessions",
  "implementation_owner": "one durable Pi session bound to the phase worktree and symbolic branch lavender/remote-hub-collector-fleet-1-docs",
  "review_independence": "a separate fresh Pi review session using thermo-nuclear-code-quality-review after implementation ownership is returned",
  "delegation_plan": [
    "implementation session inventories existing documentation conventions, implements the accepted glossary/ADRs/product/protocol updates, runs documentation gates, commits, pushes, and opens one phase PR",
    "review session challenges consistency, privacy boundaries, stale active-roadmap language, and acceptance evidence without owning Beads"
  ],
  "model_and_effort_rationale": "Use a strong general coding model with high reasoning for architecture-language consistency; phase scope is documentation-only but establishes contracts for all later phases.",
  "required_evidence": [
    "canonical glossary and four accepted ADR decisions",
    "product positioning and /api/v1 protocol/privacy invariants",
    "terminology/link scan showing no active agentless SSH-pull direction",
    "independent review outcome",
    "terminal CI state owned by the coordinator"
  ],
  "user_constraints": [
    "use the orchestrator-callback topology",
    "run phase-by-phase until complete or a real stop condition",
    "merge each phase PR into lavender/remote-hub-collector-fleet-implementation rather than main"
  ]
}
```

The coordinator owns Beads, phase advancement, CI resolution, and the integration branch. The implementation session owns only the phase worktree until structured completion returns through the adapter-bound parent channel.

## Adaptations

- The user replaced the generated `main` PR target with the new integration branch `lavender/remote-hub-collector-fleet-implementation`; Beads metadata is canonical for this execution override.

## Repair Trigger And Scout Evidence

- Coordinator review finding for PR #8: runnable or prescriptive files under `docs/implementation/refresh-remote-harness-layout-theme/` could still route actors into obsolete agentless SSH-pull work and stale `main` targeting.
- A completed read-only `pi-subagents` scout checklist was incorporated before finalization. The scout identified actionable stale surfaces in the old stream `IMPLEMENT.md`, `loop-state.md`, `00-roadmap.md`, all prompt run surfaces, `04-agentless-ssh-remote-sync.md`, and `turn-docs/dirtydash-refresh-loop.4.md`.
- The same scout also flagged `01-refresh-foundation.md`, `02-ledger-layout-reshape.md`, `03-built-in-themes.md`, `05-opencode-and-hermes-agent-harness-support.md`, and turn docs `.1`, `.2`, `.3`, `.5` as still runnable-looking despite being historical, so this repair added stronger do-not-execute redirects there as well.
- Scout evidence source: completed read-only `pi-subagents` run `79ffa927-2a48-4f37-b2e4-2ef7cef22385`, plus the coordinator summary delivered into this session.

## Discoveries And Decisions

- The repository had no separate documentation-conventions file; this phase followed the existing Markdown conventions already used in `IMPLEMENT.md`, phase docs, `README.md`, and `PRODUCT.md`.
- The active Remote Hub/Collector stream still needed explicit source-of-truth links for glossary, ADRs, and `/api/v1` invariants.
- The prior `dirtydash-refresh-loop` docs still read like an active roadmap; phase 1 marks that stream and its SSH-pull phase artifacts as superseded without erasing them.
- The active stream's branch/PR constraint now explicitly targets `lavender/remote-hub-collector-fleet-implementation`, matching the user override already recorded by the coordinator.

## Implementation And Delegation Evidence

- One durable implementation session owned `lavender/remote-hub-collector-fleet-1-docs` for the bounded documentation/tracker-record scope.
- The original phase-1 docs pass used no helper swarms because the work was a tightly scoped documentation pass rather than multi-slice implementation.
- After the independent review blocker was reported, this repair pass incorporated a completed read-only `pi-subagents` scout checklist instead of launching new mutable helpers.
- The session added the canonical glossary, four ADRs, `/api/v1` protocol/privacy invariants, product-positioning updates, and supersession notes on the old SSH-pull stream before preparing a single phase PR.
- The repair pass then made every remaining old-stream run/selection/review/PR surface operationally non-runnable while preserving historical context and forwarding actors to `docs/implementation/remote-hub-collector-fleet/` and `dirtydash-px3`.

## Changed Behavior And Files

- Added canonical domain records:
  - `docs/implementation/remote-hub-collector-fleet/CONTEXT.md`
  - `docs/implementation/remote-hub-collector-fleet/API_V1_INVARIANTS.md`
  - `docs/implementation/remote-hub-collector-fleet/adr/ADR-0001-hub-collector-topology.md`
  - `docs/implementation/remote-hub-collector-fleet/adr/ADR-0002-metadata-only-privacy-boundary.md`
  - `docs/implementation/remote-hub-collector-fleet/adr/ADR-0003-tailscale-and-fallback-administrator-authentication.md`
  - `docs/implementation/remote-hub-collector-fleet/adr/ADR-0004-sqlite-repository-seam.md`
- Cross-linked the active stream docs and phase docs to those canonical records:
  - `docs/implementation/remote-hub-collector-fleet/IMPLEMENT.md`
  - `docs/implementation/remote-hub-collector-fleet/00-roadmap.md`
  - `docs/implementation/remote-hub-collector-fleet/01-documentation-and-tracker-reset.md`
  - `docs/implementation/remote-hub-collector-fleet/02-storage-and-protocol-foundation.md`
  - `docs/implementation/remote-hub-collector-fleet/03-collector-runtime.md`
  - `docs/implementation/remote-hub-collector-fleet/04-hub-deployment-and-enrollment.md`
  - `docs/implementation/remote-hub-collector-fleet/07-migration-backup-and-release-hardening.md`
- Updated product positioning and roadmap copy:
  - `README.md`
  - `PRODUCT.md`
- Marked the old SSH-pull stream as superseded, preserving history but removing active-roadmap ambiguity:
  - `docs/implementation/refresh-remote-harness-layout-theme/IMPLEMENT.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/00-roadmap.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/loop-state.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/04-agentless-ssh-remote-sync.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.4.md`
- Strengthened the remaining historical phase and turn docs so they no longer look executable:
  - `docs/implementation/refresh-remote-harness-layout-theme/01-refresh-foundation.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/02-ledger-layout-reshape.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/03-built-in-themes.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/05-opencode-and-hermes-agent-harness-support.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.1.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.2.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.3.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.5.md`
- Neutralized old runnable prompt surfaces that could still launch obsolete work or stale `main` PR targeting:
  - `docs/implementation/refresh-remote-harness-layout-theme/prompts/run-loop.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/prompts/implementation-thread.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/prompts/review-thread.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/prompts/selector-subagent.md`
  - `docs/implementation/refresh-remote-harness-layout-theme/prompts/closeout-selector.md`
- Preserved coordinator-owned run-context updates already present in:
  - `docs/implementation/remote-hub-collector-fleet/loop-state.md`

## Review

Approved.

- The first independent `thermo-nuclear-code-quality-review` session returned `changes-required`: old refresh-loop prompts and phase records still provided runnable SSH-pull and stale `main` instructions.
- Repair commit `3d42714` hard-stopped every old run, selection, review, PR, phase, and turn-doc surface while retaining historical evidence.
- A focused read-only `pi-subagents` verification scout found no remaining blockers and confirmed redirects consistently target `dirtydash-px3`.
- A fresh independent re-review approved the repair. It found no active SSH-pull/main execution path, no broken redirect, and no scope widening.
- Residual review risk: very low; the coordinator independently reran the shell-based gates unavailable to the tool-constrained reviewer.

## CI And Gates

Owner: coordinator

State: `ci-unavailable-with-evidence`

Evidence:

- GitHub reported an empty `statusCheckRollup` for PR #8; no repository CI checks were configured for this PR.
- Coordinator reran `git diff --check origin/lavender/remote-hub-collector-fleet-implementation...HEAD`: passed.
- Coordinator verified hard-stop banners on all 19 historical runnable surfaces: passed.
- Coordinator ran relative Markdown-link validation across all 35 changed Markdown files: passed.
- Independent implementation and review terminology scans found no active agentless SSH-pull or stale `main` execution path: passed.
- `cargo test` and `npm --prefix dashboard run build` were intentionally not run because no executable or generated product surfaces changed.

## PR And Commits

- Initial implementation: `b1f1483` — `docs: define remote hub collector phase-1 canon`
- Handoff evidence: `dfae71f` — `docs: record phase-1 handoff evidence`
- Review repair: `3d42714` — `docs: hard-stop superseded refresh loop`
- PR #8: `dirtydash-px3.1: define remote hub/collector docs canon`
- PR URL: https://github.com/dirtydishes/dirtydash/pull/8
- PR target: `lavender/remote-hub-collector-fleet-implementation`
- Merged: 2026-07-15 at merge commit `98f3453`.

## Beads Updates And Follow-Ups

- Beads metadata records the user-overridden integration-branch policy.
- `dirtydash-px3.1` closed after acceptance, independent review, gate evidence, and PR merge.
- Phase 2 (`dirtydash-px3.2`) is now ready.
- No Phase 1 follow-up issues were required.

## Plan Amendments

None. The work stayed within phase-1 documentation/tracker-record scope.

## Context To Keep

- The old remote-pull roadmap is superseded, but its historical docs remain useful evidence once clearly marked non-runnable.
- `CONTEXT.md`, `API_V1_INVARIANTS.md`, and the four ADRs are the canonical phase-1 records that later phases should cite instead of restating the same decisions.
- The completed read-only `pi-subagents` scout checklist should be treated as supporting evidence for the PR #8 repair, not as a mutable source artifact to commit.
- The orchestrator launches separate durable implementation and independent review sessions; bounded `pi-subagents` scouts may support either session with read-only evidence.

## Closeout

Phase 1 complete. Acceptance evidence is present, the original review blocker was repaired, independent re-review approved, local gates passed, unavailable CI is documented, Beads is closed, and PR #8 is merged into `lavender/remote-hub-collector-fleet-implementation`.
