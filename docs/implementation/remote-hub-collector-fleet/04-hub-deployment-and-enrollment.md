# Phase 4: Hub Deployment and Enrollment

Canonical Beads issue: `dirtydash-px3.4`

Epic: `dirtydash-px3`

Status is tracked in Beads. This document preserves accepted intent and is decision-complete, implementation-open.

## Outcome

Users can install a signed Hub and local Collector, expose the Hub safely through Tailscale or a public proxy, and enroll Linux/macOS machines through explicit, recoverable Hub-side SSH steps.

## Why This Phase Exists

The storage and Collector foundations become a product only when installation, identity, services, secrets, and platform differences are handled safely.

## Scope

Allowed:

- Signed Linux/macOS x86_64/arm64 artifacts.
- `dirtydash deploy hub <ssh-target>`, platform detection, optional database seed handoff, Hub local Collector, systemd and launchd services.
- Tailscale Serve and fallback HTTPS listener configuration.
- Five-step enrollment with SSH alias/manual connection, fingerprint confirmation, transient authentication and sudo, review, install/backfill/receipt verification.
- Dirtydash-managed known hosts, changed-key blocking, retry and cleanup, and legacy remote conversion to un-enrolled drafts.

Out of scope:

- Automatic legacy-remote enrollment.
- Root-run services, unsigned self-updates, or mobile administration.

## Constraints

- SSH and sudo passwords exist only in request memory and never in arguments, environment variables, persisted jobs, logs, or diagnostics.
- Services run as the selected non-root user.
- First-time Tailscale HTTPS consent may require user action.
- Failures remain on the current wizard step with actionable output.

## Settled Decisions

Tailscale Serve is the default private entry point; public access uses fallback administrator login; SSH aliases and key paths resolve on the Hub host. See `ADR-0003: Tailscale And Fallback Administrator Authentication` for the canonical trust-mode boundary.

## Open Questions

None.

## Dependencies

- Depends on: `dirtydash-px3.3`
- Parallel-safe: no.

## Acceptance Evidence

- Installer tests cover Linux/macOS, x86_64/arm64 selection, alias/manual SSH, password/key authentication, sudo failure, restart, rollback, and cleanup.
- Security tests cover unknown/changed host keys, secret redaction, and unsigned update rejection.
- Fresh Hub deployment completes through one CLI flow apart from Tailscale consent.

## Quality Gates

- `cargo test`
- Signed artifact verification and installer/service integration tests.
- Real or isolated platform smoke checks for systemd and launchd.

## Replanning Triggers

Replan if signed distribution cannot cover required platforms, Tailscale identity cannot be verified at the listener boundary, or transient deployment secrets appear in persistence or logs.

## Implementation Hypotheses

These are suggestions to validate against repository evidence, not mandatory choreography.

- Model enrollment as a durable state machine whose secret-bearing operations remain memory-only.
- Keep installation plans inspectable before any remote mutation.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
