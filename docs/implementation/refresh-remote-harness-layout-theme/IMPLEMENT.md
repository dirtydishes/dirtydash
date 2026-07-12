# Refresh, Remote Sync, Harness, Layout, And Theme Implementation Loop

Dirtyloop version: `2`

Execution policy: `adaptive`

Canonical tracker: Beads epic `dirtydash-refresh-loop`

Accepted plan: `docs/implementation/refresh-remote-harness-layout-theme/00-roadmap.md`

Beads owns state. These docs preserve accepted intent and execution context.

## Goal

Build the finalized Dirtydash refresh, remote sync, harness, Ledger layout, and theme upgrade as a bounded five-phase stream. The stream implements explicit refresh first, then Ledger reshaping, themes, agentless SSH remote import, and OpenCode/Hermes harness support. Live watcher/SSE work is intentionally future scope and is tracked separately as `dirtydash-live-watcher-future`.

## Scope And Non-Goals

See the accepted plan and phase docs.

## Settled Decisions

See the accepted plan and phase docs.

## Sources Of Truth

- Beads epic: `dirtydash-refresh-loop`
- Accepted plan: `docs/implementation/refresh-remote-harness-layout-theme/00-roadmap.md`
- Roadmap: `docs/implementation/refresh-remote-harness-layout-theme/00-roadmap.md`
- Phase docs linked from Beads
- Turn docs: `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/`
- Resume mirror: `docs/implementation/refresh-remote-harness-layout-theme/loop-state.md`

## Control-Plane Invariants

- Select one ready phase unless the accepted plan explicitly permits parallel phases.
- Read its phase doc and write an orchestration brief before broad work.
- Choose model, effort, topology, delegation, concurrency, and coordination from current evidence and capabilities.
- Keep one owner per mutable checkout and verify repo/worktree/symbolic branch before child mutation or review.
- Bind callback targets at run time when callbacks are used.
- Use independent review and resolve CI before completion.
- Update the existing phase turn doc and Beads; file follow-ups instead of widening scope.

## Phase Ledger

| Beads Issue | Phase | Phase Doc | Depends On | Status |
|---|---|---|---|---|
| `dirtydash-refresh-loop.1` | Refresh Foundation | `docs/implementation/refresh-remote-harness-layout-theme/01-refresh-foundation.md` | none | open |
| `dirtydash-refresh-loop.2` | Ledger Layout Reshape | `docs/implementation/refresh-remote-harness-layout-theme/02-ledger-layout-reshape.md` | `dirtydash-refresh-loop.1` | open |
| `dirtydash-refresh-loop.3` | Built-In Themes | `docs/implementation/refresh-remote-harness-layout-theme/03-built-in-themes.md` | `dirtydash-refresh-loop.2` | open |
| `dirtydash-refresh-loop.4` | Agentless SSH Remote Sync | `docs/implementation/refresh-remote-harness-layout-theme/04-agentless-ssh-remote-sync.md` | `dirtydash-refresh-loop.3` | open |
| `dirtydash-refresh-loop.5` | OpenCode And Hermes Agent Harness Support | `docs/implementation/refresh-remote-harness-layout-theme/05-opencode-and-hermes-agent-harness-support.md` | `dirtydash-refresh-loop.4` | open |

## Quality Gates

- `cargo test`
- `npm --prefix dashboard run build`
- Focused Rust importer/API tests for backend phases
- Browser smoke for UI phases and any dashboard-visible refresh/remote status changes
- `git diff --check`

## Branch And PR Constraints

- One active phase PR at a time.
- Implementation branches use `lavender/dirtydash-refresh-loop-<phase-slug>`.
- Branches start from `main` unless the orchestrator intentionally chooses a more current base after closeout.
- PRs target `main` on `origin`.
- Workers own their phase branch, gates, PR creation, turn doc implementation notes, and exactly one implementation callback.
- Reviewers own strict review, CI inspection, safe in-scope repairs, reruns, turn doc review/CI notes, and exactly one review callback.
- The orchestrator alone closes Beads issues, updates `loop-state.md`, selects the next phase, and performs stream closeout.
