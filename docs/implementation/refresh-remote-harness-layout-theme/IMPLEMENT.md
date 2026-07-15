# Refresh, Remote Sync, Harness, Layout, And Theme Implementation Loop

> **Superseded implementation loop — stop:** preserved for history only. Do not run this loop, launch child threads from these prompts, or use this file for branch/PR decisions. Active remote implementation is `dirtydash-px3` in `docs/implementation/remote-hub-collector-fleet/`, which replaces the agentless SSH-pull direction with the accepted Hub/Collector fleet.

Workflow: `orchestrator-callback`

Canonical tracker: Beads epic `dirtydash-refresh-loop`

This stream is driven by Beads. These docs are execution context and resume aids. If Beads and these docs disagree, Beads wins.

## Goal

Historical goal only: build the finalized Dirtydash refresh, remote sync, harness, Ledger layout, and theme upgrade as a bounded five-phase stream. This stream is superseded for remote architecture purposes; active implementation moved to the Hub/Collector fleet stream, while Live watcher/SSE remains future scope as `dirtydash-live-watcher-future`.

## Sources Of Truth

- Beads epic: `dirtydash-refresh-loop`
- Beads loop metadata: workflow, run policy, branch/PR policy, quality gates, callback policy, actor-specific thread defaults, and implementation swarm policy
- Roadmap: `docs/implementation/refresh-remote-harness-layout-theme/00-roadmap.md`
- Loop state mirror: `docs/implementation/refresh-remote-harness-layout-theme/loop-state.md`
- Phase docs linked from Beads child issues
- Turn docs: `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/`

## Historical Loop Rules

Preserved below only to explain the former workflow. Do not execute these instructions; use the active `docs/implementation/remote-hub-collector-fleet/` run surfaces instead.

- Select exactly one next ready Beads child issue.
- Follow the selector gate before launching implementation.
- Read the linked phase doc before editing.
- Continue phase-by-phase by default until the epic is complete, blocked, interrupted, or review/CI is unresolved.
- Use `run once` / `--once` only when intentionally running one phase and stopping after closeout.
- Keep one active implementation PR at a time.
- File Beads follow-ups instead of widening the selected phase.
- Update Beads first, then update `loop-state.md`.
- Child threads default to `speed: standard`, `reasoning: xhigh`, and `inherit_orchestrator_thread_settings: false`.
- Implementation/review threads must be created in the intended Codex project worktree. A child that starts in the wrong repo/worktree blocks and calls back instead of self-relocating.
- Orchestrator-callback is callback-wait. After launching a worker or reviewer, wait for exactly one callback and use only sparse fallback heartbeat when overdue or liveness is uncertain.
- Generated worker/reviewer prompts store `RUNTIME_ORCHESTRATOR_THREAD_ID` until a run binds the concrete orchestrator thread id.
- Worker/reviewer prompts must carry the literal orchestrator thread id after runtime binding. Do not launch if the prompt still contains the placeholder or uses generic callback-target wording.

## Historical Review And CI

Reviewer agents must use:

`thermo-nuclear-code-quality-review`

Reviewer and CI verification agents own CI.

Allowed CI closeout states:

- `ci-green`
- `ci-repaired-and-green`
- `ci-unavailable-with-evidence`
- `ci-blocked-with-cause`

Unknown CI is not approval.

## Historical Turn Docs

Each phase has exactly one Markdown turn doc:

`docs/implementation/refresh-remote-harness-layout-theme/turn-docs/<phase-issue-id>.md`

Implementation, review, CI, repairs, PR state, Beads updates, follow-ups, and closeout all go into the same doc.

## Historical Storyboard

When the epic is complete, generate:

`docs/implementation/refresh-remote-harness-layout-theme/storyboard-post-run-07-03-2026.html`

Use `impeccable` when present. If missing, continue without it and note that it was skipped.

Install `@pierre/diffs` in the target repo if missing. Every diff must use `@pierre/diffs/ssr`.

## Historical Phase Ledger

| Beads Issue | Phase | Phase Doc | Depends On | Status |
|---|---|---|---|---|
| `dirtydash-refresh-loop.1` | Refresh Foundation | `docs/implementation/refresh-remote-harness-layout-theme/01-refresh-foundation.md` | none | superseded |
| `dirtydash-refresh-loop.2` | Ledger Layout Reshape | `docs/implementation/refresh-remote-harness-layout-theme/02-ledger-layout-reshape.md` | `dirtydash-refresh-loop.1` | superseded |
| `dirtydash-refresh-loop.3` | Built-In Themes | `docs/implementation/refresh-remote-harness-layout-theme/03-built-in-themes.md` | `dirtydash-refresh-loop.2` | superseded |
| `dirtydash-refresh-loop.4` | Agentless SSH Remote Sync | `docs/implementation/refresh-remote-harness-layout-theme/04-agentless-ssh-remote-sync.md` | `dirtydash-refresh-loop.3` | superseded |
| `dirtydash-refresh-loop.5` | OpenCode And Hermes Agent Harness Support | `docs/implementation/refresh-remote-harness-layout-theme/05-opencode-and-hermes-agent-harness-support.md` | `dirtydash-refresh-loop.4` | superseded |

## Historical Quality Gates

- `cargo test`
- `npm --prefix dashboard run build`
- Focused Rust importer/API tests for backend phases
- Browser smoke for UI phases and any dashboard-visible refresh/remote status changes
- `git diff --check`

## Historical Branch And PR Policy

Do not use this policy for current work. It is preserved only to explain the former stream shape.

- Historical runs kept one active phase PR at a time.
- Historical implementation branches used `lavender/dirtydash-refresh-loop-<phase-slug>`.
- Historical runs started from `main` unless the orchestrator intentionally chose a more current base after closeout.
- Historical PRs targeted `main` on `origin`.
- Historical workers owned their phase branch, gates, PR creation, turn doc implementation notes, and exactly one implementation callback.
- Historical reviewers owned strict review, CI inspection, safe in-scope repairs, reruns, turn doc review/CI notes, and exactly one review callback.
- The historical orchestrator alone closed Beads issues, updated `loop-state.md`, selected the next phase, and performed stream closeout.

## Historical Related Beads

- `dirtydash-9y4`: prior live session and keyboard-first idea epic
- `dirtydash-9y4.1`: prior auto-update/live session issue
- `dirtydash-fol`: prior SSH remote usage import issue
- `dirtydash-live-watcher-future`: future live watcher/SSE follow-up, not part of this epic
