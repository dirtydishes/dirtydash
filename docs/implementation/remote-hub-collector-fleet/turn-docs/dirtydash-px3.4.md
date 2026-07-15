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

- `crates/dirtydash/src/deployment.rs`, `deployment_tests.rs`, and `ssh.rs`: durable configured publisher trust enforcement, four-target selection, persisted concrete plans, canonical SSH resolution, typed executor/runner actions, controlled-PTY credential input, portable SQLite/WAL seed replacement, actual listener/service/current-pointer rollback snapshots, old Hub/Collector rollback health, and deterministic failure evidence.
- `crates/dirtydash/src/enrollment.rs` and `enrollment_tests.rs`: durable model/workflow/store/SSH seams, canonical target/managed host-key confirmation, zeroized memory-only authentication, live password/sudo PTY execution, persisted reviewed artifact/seed intent, execution substates with cleanup/retry/manual-recovery blocker, receipt handling, and legacy remote-to-un-enrolled draft conversion.
- `crates/dirtydash/src/service.rs`: deterministic non-root systemd user and launchd service rendering for Hub plus local Collector.
- `crates/dirtydash/src/listener.rs`: Tailscale/private default, explicit consent state, public fallback trust configuration, runtime TOML, and listener command/state mapping.
- `crates/dirtydash/src/cli.rs`, `config.rs`, `lib.rs`: concrete `deploy hub <ssh-target>` plan/apply CLI, publisher allowlist, atomic restrictive secret store, Hub listener flags, and listener configuration.
- `crates/dirtydash/src/hub/{mod.rs,router.rs}`: runnable authenticated Hub listener using the connect-info trust seam and `/healthz`.
- `crates/dirtydash/tests/cli.rs`, module tests, and `docs/deployment.md`: isolated publisher replacement, plan/signature/platform/service/listener/enrollment/live-PTY/SQLite-WAL/rollback/secret-redaction coverage and fleet deployment guidance. The legacy Docker/NPM script remains untouched.

## Thermo-Nuclear Repair Pass

The independent review blockers were repaired on the phase branch. Planning now performs a concrete remote probe, persists the full artifact/facts/exposure/seed/backfill/rollback plan, and apply requires the reviewed persisted hash while recomputing facts before mutation. Publisher verification now requires a durable configured key ID/fingerprint; CLI flags can only assert that anchor, and `VerifiedArtifact` can only be produced by verification.

SSH uses one `ssh -G`-resolved typed target (HostName/Port/User/HostKeyAlias/ProxyJump) for keyscan, managed known-host lookup, and execution. First use requires an exact confirmation and changed keys remain hard failures. Bootstrap/Collector credentials moved to atomic `0600` secret storage and are absent from TOML snapshots/debug output. Seed replacement now snapshots and validates SQLite plus WAL/SHM with a Python/`od` byte-level fallback, quiesces services, uses platform-specific activation, independently verifies Hub and Collector health, and restores actual release/config/services/database/listener state on rollback.

Enrollment stores the reviewed plan and artifact/seed intent, has durable execution substates, and permits restart/retry only after cleanup with the same hash. Password-authenticated installs use the same controlled live PTY as trust/probe; fixed password, key-passphrase, and sudo prompts release bounded zeroized bytes only to live stdin. User systemd/launchd units no longer switch users, and already-loaded launchd jobs are handled explicitly. Private Tailscale binds loopback-only, while trusted-proxy CIDRs are validated at config time and transport peer/header trust is fail-closed. Rollback restores the snapshotted listener and prior service definitions, restarts both services, checks old Hub `/healthz` plus Collector diagnostics, and records a manual-recovery blocker if rollback health fails. Focused SSH, publisher replacement, secret-store, plan, byte-level SQLite/WAL, rollback/manual-recovery, listener, service, live-PTY redaction, and restart tests were added.

## Review

Pending independent fresh review of this repair pass. Live signing keys, real SSH hosts/managers, public certificates, and tailnet consent remain unavailable external gates; no evidence is fabricated.

## CI And Gates

Owner: implementation session for local gates; coordinator for independent review and terminal CI

State: local-gates-passed; live-release-evidence-unavailable

Evidence:

- `cargo fmt --all`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- Thermo-nuclear repair validation covers persisted reviewed plans, durable publisher anchoring/replacement rejection, canonical SSH/host-key trust, controlled live-PTY password/sudo success/failure/redaction, secret snapshots/permissions, no-sqlite3 byte-level valid/malformed/WAL paths, actual listener/service/current-pointer rollback, old Hub/Collector rollback health, manual-recovery status, listener CIDRs/peer trust, execution restart, and redaction.
- `cargo test --all-targets --all-features`: passed (98 unit tests, 9 CLI tests, 14 Collector integration tests).
- `npm --prefix dashboard run build`: passed with Vite production output.
- Local Hub smoke: `serve --hub --listener public --host 127.0.0.1 --port 0` started a real connect-info router and `/healthz` returned `{"service":"dirtydash-hub","status":"ok"}`.
- Live production signing, SSH alias/manual host enrollment, changed-key behavior against real hosts, systemd/launchd manager operations, Tailscale consent, public TLS certificates, and real release artifact deployment were not available in this isolated environment.

## PR And Commits

- Base implementation commit `1c84e3c` (`feat: add signed hub deployment and ssh enrollment`).
- Repair commit `c6aeb38` (`fix: harden phase 4 deployment and enrollment`).
- Final release-blocker repair commit `55c7ec3` (`fix: close phase 4 release blockers`).
- Phase PR: [#11](https://github.com/dirtydishes/dirtydash/pull/11), head `lavender/remote-hub-collector-fleet-4-deployment`, base `lavender/remote-hub-collector-fleet-implementation`.
- Branch pushed to `origin`; merge remains coordinator-owned.

## Beads Updates And Follow-Ups

Loop creation established the issue and dependency graph.

## Plan Amendments

None.

## Context To Keep

Deployment credentials are memory-only and must never enter arguments, environment, persistence, diagnostics, or logs.

## Closeout

Implementation, repair validation, commit, and phase-branch push are complete. Independent review, coordinator terminal CI, and integration-branch merge remain coordinator-owned; no Beads state was mutated and no merge was performed.
