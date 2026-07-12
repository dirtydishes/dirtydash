# Dirtydash Remote Hub and Collector Fleet Implementation Loop

Dirtyloop version: `2`

Execution policy: `adaptive-with-user-topology-constraint`

Canonical tracker: Beads epic `dirtydash-px3`

Accepted plan: user-supplied **Dirtydash Remote Hub and Collector Fleet** plan, accepted 2026-07-12

Beads owns state. These docs preserve accepted intent and execution context.

## Goal

Replace the old agentless SSH-pull roadmap with a push-based fleet: a central Hub owns the canonical database and dashboard, lightweight Collectors parse locally and push normalized metadata, and `dirtydash serve` keeps the loopback-only all-in-one experience. A user must be able to deploy a Hub, enroll another machine, and normally see new usage within ten seconds without manually importing files.

## Scope And Non-Goals

In scope: authenticated Hub and metadata-only Collector protocol, SQLite repository seam, local manifests and durable outboxes, deployment and enrollment, fleet operations, the Usage/Machines/Settings product surface, themes, migration, backup, restore, and release proof on Linux and macOS.

Non-goals: PostgreSQL adapter implementation; centrally stored prompts, responses, raw session files, absolute paths, SSH passwords, or sudo passwords; inbound access to Collectors; automatic enrollment of legacy remotes; mobile enrollment or destructive administration; ambient Matrix code-rain; unrelated Agent families.

## Settled Decisions

- The Hub is canonical; every machine, including the Hub machine, runs a Collector.
- `dirtydash serve` remains loopback-only and requires no account setup.
- Collectors deliver versioned, idempotent, transactional batches at least once through `/api/v1`, buffer unacknowledged events locally, reconcile every fifteen minutes, and long-poll for owner commands.
- Stable event identity includes Machine ID, Agent, and Collector event fingerprint.
- SQLite in WAL mode is V1 storage behind a narrow repository boundary; ingestion writes are serialized.
- Tailscale Serve is the default private HTTPS entry point. Public reverse proxies ignore Tailscale headers and require fallback administrator sessions.
- The Hub stores hashed Collector credentials and Argon2id administrator credentials. Deployment secrets live only in request memory.
- Only Machines are manually enrolled; Agents, projects, and models are discovered from usage.
- The primary information architecture is Usage, Machines, and Settings.
- Dirtydash Mono is the default theme; all themes share semantic OKLCH tokens and meet WCAG AA.
- One active implementation PR is allowed at a time, in phase order.
- No consequential product or architecture questions remain unresolved.
- Replan when security assumptions, Tailscale identity behavior, SQLite ingestion limits, cross-version compatibility, release signing, or Linux/macOS service installation invalidate the accepted architecture or phase ordering.

## Stream Acceptance Evidence

- A fresh Hub deploy succeeds through one CLI flow apart from required Tailscale consent.
- A Machine is enrolled entirely through the hosted UI using Hub-side SSH.
- Existing local history seeds the Hub without manual file imports.
- New usage normally appears within ten seconds; offline usage arrives after reconnection without duplication.
- No prohibited raw content, absolute path, SSH password, or sudo password reaches persistent Hub storage.
- The Hub survives restart, restores from a verified backup, and rolls back a failed update.
- The real-tailnet release gate passes with one Linux Hub, a second Linux Collector, and a macOS Collector.

## Sources Of Truth

- Beads epic: `dirtydash-px3`
- Accepted plan: user-supplied plan accepted 2026-07-12
- Roadmap: `docs/implementation/remote-hub-collector-fleet/00-roadmap.md`
- Phase docs linked from Beads
- Turn docs: `docs/implementation/remote-hub-collector-fleet/turn-docs/`
- Resume mirror: `docs/implementation/remote-hub-collector-fleet/loop-state.md`

## Control-Plane Invariants

- Select one ready phase; phases execute sequentially.
- Read its phase doc and write an orchestration brief before broad work.
- Use the explicitly requested `orchestrator-callback` topology. The orchestrator owns Beads and phase transitions; callback targets are bound to the concrete run-time orchestrator thread before child launch.
- Within that topology, the run-time orchestrator chooses model, effort, delegation, concurrency, role decomposition, and coordination details from current evidence and capabilities.
- Keep one owner per mutable checkout and verify repo, worktree, and symbolic branch before child mutation or review.
- Keep one active implementation PR.
- Use independent review and resolve CI before completion.
- Update the existing phase turn doc and Beads; file follow-ups instead of widening scope.
- Continue phase-by-phase unless complete, blocked, interrupted, unresolved, or explicitly `--once`.

## Phase Ledger

| Beads Issue | Phase | Outcome | Phase Doc | Depends On | Status |
|---|---|---|---|---|---|
| `dirtydash-px3.1` | 1 | Documentation and tracker reset | `01-documentation-and-tracker-reset.md` | none | open |
| `dirtydash-px3.2` | 2 | Storage and protocol foundation | `02-storage-and-protocol-foundation.md` | `dirtydash-px3.1` | open |
| `dirtydash-px3.3` | 3 | Collector runtime | `03-collector-runtime.md` | `dirtydash-px3.2` | open |
| `dirtydash-px3.4` | 4 | Hub deployment and enrollment | `04-hub-deployment-and-enrollment.md` | `dirtydash-px3.3` | open |
| `dirtydash-px3.5` | 5 | Fleet management | `05-fleet-management.md` | `dirtydash-px3.4` | open |
| `dirtydash-px3.6` | 6 | Usage redesign and themes | `06-usage-redesign-and-themes.md` | `dirtydash-px3.5` | open |
| `dirtydash-px3.7` | 7 | Migration, backup, and release hardening | `07-migration-backup-and-release-hardening.md` | `dirtydash-px3.6` | open |

## Quality Gates

- `cargo test`
- `npm --prefix dashboard run build`
- Phase-specific protocol, security, database, importer, installer, and frontend tests
- Browser accessibility and responsive smoke checks for UI phases
- Signed-artifact and service-install verification for Linux/macOS x86_64/arm64
- Real-tailnet release gate in phase 7

## Branch And PR Constraints

One active phase PR at a time, in phase order. Branches use `lavender/remote-hub-collector-fleet-<phase>` and target `main` on `origin`. Beads, review, CI, repairs, and PR state remain in the phase's single turn doc.

## Storyboard

On epic completion, generate `docs/implementation/remote-hub-collector-fleet/storyboard-post-run-07-12-2026.html`. Use `impeccable` when available and `@pierre/diffs/ssr` for every diff.
