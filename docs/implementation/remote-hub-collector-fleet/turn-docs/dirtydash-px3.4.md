# Phase 4 Turn Doc: Hub Deployment and Enrollment

Beads issue: `dirtydash-px3.4`

Phase doc: `docs/implementation/remote-hub-collector-fleet/04-hub-deployment-and-enrollment.md`

## Accepted Outcome

Signed Hub/Collector artifacts deploy safely and machines enroll through explicit Hub-side SSH steps.

## Orchestration Brief

```json
{
  "phase_issue_id": "dirtydash-px3.4",
  "risk": "high",
  "strategy": "sessions",
  "implementation_owner": "one durable Luna-max Pi implementation session bound to lavender/remote-hub-collector-fleet-4-deployment",
  "review_independence": "a separate fresh Luna-max Pi review session using thermo-nuclear-code-quality-review after implementation ownership returns",
  "delegation_plan": [
    "Sol-low read-only pi-subagents scouts inventory existing deployment/release surfaces and SSH/secret/service security boundaries",
    "the durable implementation session owns all code/test/script/service mutations and one phase PR",
    "fresh review receives separate Sol-low installer/security evidence; bounded repairs return to one mutable owner"
  ],
  "model_and_effort_rationale": "Remote mutation, host-key trust, transient credentials, non-root services, signed artifacts, and rollback behavior justify Luna max for implementation/review and bounded Sol-low evidence scouts.",
  "required_evidence": [
    "Linux/macOS x86_64/arm64 artifact selection and signature verification",
    "inspectable deployment plan plus non-root systemd/launchd services and rollback",
    "Tailscale Serve/public listener trust-mode configuration",
    "five-step SSH enrollment state machine with known-host confirmation/change blocking",
    "secret non-persistence across args/env/jobs/logs/diagnostics",
    "alias/manual/key/password/sudo failure/retry/cleanup tests",
    "fresh Hub deployment smoke or unavailable evidence",
    "fmt, clippy, cargo test, independent review, and terminal CI state"
  ],
  "user_constraints": [
    "separate implementation and independent review sessions",
    "parent-mediated Sol-low pi-subagents scouts",
    "merge the phase PR into lavender/remote-hub-collector-fleet-implementation before advancing"
  ]
}
```

The implementation session is sole mutable owner of the phase worktree. The coordinator owns Beads, integration, CI, and callbacks; scouts are read-only.

## Adaptations

- The phase PR targets the integration branch rather than `main`.
- Production signing credentials and real remote hosts are external evidence dependencies; implementation must provide deterministic local/isolated verification and report any unavailable real-environment gate rather than inventing evidence.

## Discoveries And Decisions

- Accepted deployment/release scout `3a863689-78ee-4ac1-98fb-417fddee9ed0`: keep the historical Docker/NPM helper unchanged; use a narrow typed deployment plan/probe/executor/verifier seam with pure platform mapping, user-owned versioned paths, atomic activation, rollback, and secret-free serializable plans.
- Accepted SSH/security scout `ec3e3719-d939-416b-ac1a-a2d4b6592d7f`: keep enrollment separate from the legacy `remote.rs` pull surface; persist only sanitized state, use Dirtydash-managed known hosts with strict checking, and use inherited PTY/stdin for password/passphrase/sudo operations.
- Release signatures use Ed25519 over a canonical manifest payload plus per-artifact SHA-256 and size verification. Linux/macOS x86_64/arm64 selection is deterministic; production signing keys remain external.
- Tailscale Serve is represented as a resumable `consent-required` state. Public HTTPS requires fallback administrator sessions and optional transport-bound trusted-proxy configuration; no Tailscale header is trusted on a public listener without the existing Hub trust seam.
- Enrollment has five durable sanitized states (target draft, host trust/auth, probe/plan, immutable review, execute/verify/receipt), unknown-key confirmation, changed-key blocking, plan-hash invalidation, retry/cleanup substates, and no secret-bearing persisted field.

## Implementation And Delegation Evidence

The implementation owner incorporated both parent-mediated read-only Sol-low scout outputs above. No scout mutated this worktree or Beads. Real release signing keys, live SSH hosts, systemd/launchd managers, public certificates, and tailnet consent were not available in the isolated environment and are reported as external evidence dependencies.

## Changed Behavior And Files

- `crates/dirtydash/src/deployment.rs`: signed manifest verifier, four-target platform selection, typed inspectable deployment plan, remote probe/executor, atomic user-owned release activation, optional SQLite seed backup/restore, non-root service installation, restart/health/receipt checks, Tailscale consent checkpoint, rollback, and cleanup.
- `crates/dirtydash/src/enrollment.rs`: durable five-state SSH enrollment workflow, alias/manual target validation, managed known-host fingerprint confirmation/change blocking, memory-only zeroized secret inputs, fixed SSH operations, sanitized durable drafts, retry/cleanup, receipt/backfill handling, and legacy remote-to-un-enrolled draft conversion.
- `crates/dirtydash/src/service.rs`: deterministic non-root systemd user and launchd service rendering for Hub plus local Collector.
- `crates/dirtydash/src/listener.rs`: Tailscale/private default, explicit consent state, public fallback trust configuration, runtime TOML, and listener command/state mapping.
- `crates/dirtydash/src/cli.rs`, `config.rs`, `lib.rs`: `deploy hub <ssh-target>` plan/apply CLI, Hub listener flags, and listener configuration.
- `crates/dirtydash/src/hub/{mod.rs,router.rs}`: runnable authenticated Hub listener using the connect-info trust seam and `/healthz`.
- `crates/dirtydash/tests/cli.rs`, module tests, and `docs/deployment.md`: isolated plan/signature/platform/service/listener/enrollment/secret-redaction coverage and fleet deployment guidance. The legacy Docker/NPM script remains untouched.

## Review

Pending independent fresh review. The implementation owner has not self-closed review findings; coordinator review and any bounded repairs remain outstanding.

## CI And Gates

Owner: implementation session for local gates; coordinator for independent review and terminal CI

State: local-gates-passed; live-release-evidence-unavailable

Evidence:

- `cargo fmt --all`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo test --all-targets`: passed (76 unit tests, 7 CLI tests, 14 Collector integration tests).
- Local Hub smoke: `serve --hub --listener public --port 0` started a real connect-info router and `/healthz` returned `{"service":"dirtydash-hub","status":"ok"}`.
- Live production signing, SSH alias/manual host enrollment, changed-key behavior against real hosts, systemd/launchd manager operations, Tailscale consent, public TLS certificates, and real release artifact deployment were not available in this isolated environment.

## PR And Commits

Pending commit/push/phase PR creation.

## Beads Updates And Follow-Ups

Loop creation established the issue and dependency graph.

## Plan Amendments

None.

## Context To Keep

Deployment credentials are memory-only and must never enter arguments, environment, persistence, diagnostics, or logs.

## Closeout

Implementation and local validation are complete. Independent review, coordinator terminal CI, commit/push, and one PR targeting `lavender/remote-hub-collector-fleet-implementation` remain for handoff.
