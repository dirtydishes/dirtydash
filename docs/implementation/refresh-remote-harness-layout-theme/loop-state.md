# Loop State

Canonical tracker: Beads epic `dirtydash-refresh-loop`

This file is a compact resume aid only. If this file disagrees with Beads, Beads wins.

Status: active

Stream: `refresh-remote-harness-layout-theme`

Workflow: `orchestrator-callback`

Current phase: Refresh Foundation

Current Beads issue: `dirtydash-refresh-loop.1`

Current PR: none

Last completed phase: none

Blocked: no

## Decisions

- In-scope phases are plan phases 1-5: refresh foundation, Ledger layout reshape, themes, agentless SSH remote sync, and OpenCode/Hermes harness support.
- Plan phase 6, live watcher/SSE, is explicitly future scope and filed as `dirtydash-live-watcher-future`.
- The run orchestrator must capture its own concrete Codex thread id before launching child threads.
- Child-thread prompt templates use `RUNTIME_ORCHESTRATOR_THREAD_ID` until the run orchestrator replaces it with that concrete id.
- Orchestrator-callback child threads use `speed: standard`, `reasoning: xhigh`, and `inherit_orchestrator_thread_settings: false`.

## Context To Keep

- Server routes live in `crates/dirtydash/src/server.rs`.
- CLI import/serve/remote command entry points live in `crates/dirtydash/src/cli.rs`.
- Source kind, scanning, parsing, and import logic lives in `crates/dirtydash/src/importers.rs`.
- Remote sync currently discovers file counts in `crates/dirtydash/src/remote.rs`.
- Dashboard UI and keyboard state live in `dashboard/src/main.tsx`; styling and token work live in `dashboard/src/styles.css`.
- Existing related Beads are `dirtydash-9y4`, `dirtydash-9y4.1`, and `dirtydash-fol`.

## Phase Ledger

| Phase | Beads Issue | Status | PR | Turn Doc |
|---|---|---|---|---|
| Refresh Foundation | `dirtydash-refresh-loop.1` | open | none | `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.1.md` |
| Ledger Layout Reshape | `dirtydash-refresh-loop.2` | open | none | `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.2.md` |
| Built-In Themes | `dirtydash-refresh-loop.3` | open | none | `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.3.md` |
| Agentless SSH Remote Sync | `dirtydash-refresh-loop.4` | open | none | `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.4.md` |
| OpenCode And Hermes Agent Harness Support | `dirtydash-refresh-loop.5` | open | none | `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/dirtydash-refresh-loop.5.md` |

## Last Coordinator Update

2026-07-05: Phase 1 relaunched after the previous failed implementation run. Active orchestrator callback target is `019f3380-c230-7a10-8778-02b12578ba33`; active implementation thread is `019f3383-675b-7420-a529-a0fbebac77fb` in Dirtydash project worktree `/home/delta/.codex/worktrees/48b5/dirtydash` on branch `lavender/dirtydash-refresh-loop-1-refresh-foundation`.
