# Loop State

Canonical tracker: Beads epic `dirtydash-px3`

This file is a compact resume aid only. If this file disagrees with Beads, Beads wins.

Status: active

Stream: `remote-hub-collector-fleet`

Execution policy: `adaptive-with-user-topology-constraint`

Harness: `pi`

Adapter contract: `dirtyloops-harness/1`

Current phase: none; Phase 4 is ready but not yet claimed

Current Beads issue: none

Current PR: none

Current execution strategy: none between phases

Last completed phase: 3 — Collector runtime (`dirtydash-px3.3`, PR #10)

Blocked: no

## Decisions

- Use the user-requested `orchestrator-callback` topology; bind callbacks at run time.
- Keep model, effort, delegation, concurrency, and role decomposition adaptive.
- Keep one active phase PR and one owner per mutable checkout.
- The user overrode the generated PR target: phase PRs merge into `lavender/remote-hub-collector-fleet-implementation` before advancement.
- Beads owns phase state and sequencing.

## Context To Keep

- The old `dirtydash-refresh-loop` roadmap and its five children were superseded by this stream.
- The Beads Dolt database was recovered from the canonical remote, migrated to v53 by the designated migrator, and republished before loop creation.
- No consequential planning questions remain unresolved.

## Phase Ledger

| Phase | Beads Issue | Status | PR | Turn Doc |
|---|---|---|---|---|
| 1 | `dirtydash-px3.1` | closed | #8 merged (`98f3453`) | `turn-docs/dirtydash-px3.1.md` |
| 2 | `dirtydash-px3.2` | closed | #9 merged (`5dd6b70`) | `turn-docs/dirtydash-px3.2.md` |
| 3 | `dirtydash-px3.3` | closed | #10 merged (`68e4e55`) | `turn-docs/dirtydash-px3.3.md` |
| 4 | `dirtydash-px3.4` | open | none | `turn-docs/dirtydash-px3.4.md` |
| 5 | `dirtydash-px3.5` | open | none | `turn-docs/dirtydash-px3.5.md` |
| 6 | `dirtydash-px3.6` | open | none | `turn-docs/dirtydash-px3.6.md` |
| 7 | `dirtydash-px3.7` | open | none | `turn-docs/dirtydash-px3.7.md` |

## Last Coordinator Update

Phase 3 closed after privacy/reliability review and three repair passes. PR #10 merged into the integration branch; Phase 4 is ready for selection.
