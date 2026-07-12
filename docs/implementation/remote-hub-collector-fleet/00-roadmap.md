# Dirtydash Remote Hub and Collector Fleet Roadmap

Canonical tracker: Beads epic `dirtydash-px3`

## Plan Source

User-supplied **Dirtydash Remote Hub and Collector Fleet** plan, accepted 2026-07-12. The user confirmed that no consequential questions remain unresolved and accepted the replanning triggers below.

## Outcome

Ship a self-hosted Hub plus push-based Collector fleet while preserving loopback-only local use and the metadata-only privacy boundary.

## Phase Sequence

1. `dirtydash-px3.1` — reset documentation and tracker language.
2. `dirtydash-px3.2` — establish Hub storage, access, and `/api/v1` ingestion.
3. `dirtydash-px3.3` — build the local Collector pipeline and durable delivery.
4. `dirtydash-px3.4` — ship artifacts, Hub deployment, services, and enrollment jobs.
5. `dirtydash-px3.5` — implement Machine lifecycle and fleet updates.
6. `dirtydash-px3.6` — replace the dashboard with the fleet Usage experience and themes.
7. `dirtydash-px3.7` — seed history, prove backups and restore, deprecate legacy pull, and pass release gates.

## Dependencies

The graph is strictly sequential. Each phase depends on the prior phase because protocol and storage contracts precede Collector delivery; Collector behavior precedes deployment; deployment precedes fleet operations; stable fleet state precedes the Usage redesign; release migration and proof close the complete system.

## Settled Decisions

- Push-based Hub/Collector topology replaces agentless SSH pull.
- All persisted and transported usage is metadata-only.
- SQLite WAL plus a repository seam is V1; PostgreSQL is deferred.
- Tailscale Serve and local administrator sessions are separate trust modes.
- Collectors are outbound-only and use durable at-least-once delivery.
- Usage, Machines, and Settings replace the old four-workspace rail.
- One active phase PR executes at a time.
- `orchestrator-callback` is the explicit coordination topology; all other execution choices remain adaptive.

## Open Questions

None remain at loop creation. Implementation discoveries that challenge accepted intent trigger replanning instead of silent redesign.

## Risks

- Trust-boundary mistakes between Tailscale and public listeners.
- SQLite write contention or time-zone rebucketing errors.
- Parser identity drift causing duplicate fleet events.
- Secret leakage in deployment jobs or diagnostics.
- Cross-version rollout and rollback failures.
- Platform divergence across systemd, launchd, Linux, macOS, x86_64, and arm64.
- Live ledger updates disrupting keyboard selection or accessibility.

## Replanning Triggers

Replan if security assumptions, Tailscale identity behavior, SQLite ingestion limits, cross-version compatibility, release signing, or Linux/macOS service installation invalidate the accepted architecture or phase ordering. Phase-specific triggers appear in each phase doc.

## Quality Gates

`cargo test`, `npm --prefix dashboard run build`, focused protocol/security/database/importer/installer/frontend tests, browser accessibility and responsive smoke checks, signed artifact verification, and the final real-tailnet deployment gate.

## Closeout

The final closeout artifact is:

`docs/implementation/remote-hub-collector-fleet/storyboard-post-run-07-12-2026.html`
