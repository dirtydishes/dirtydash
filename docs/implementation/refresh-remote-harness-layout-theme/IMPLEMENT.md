# Refresh, Remote Sync, Harness, Layout, And Theme Implementation Loop

Workflow: `orchestrator-callback`

Canonical tracker: Beads epic `dirtydash-refresh-loop`

This stream is driven by Beads. These docs are execution context and resume aids. If Beads and these docs disagree, Beads wins.

## Goal

Build the finalized Dirtydash refresh, remote sync, harness, Ledger layout, and theme upgrade as a bounded five-phase stream. The stream implements explicit refresh first, then Ledger reshaping, themes, agentless SSH remote import, and OpenCode/Hermes harness support. Live watcher/SSE work is intentionally future scope and is tracked separately as `dirtydash-live-watcher-future`.

## Sources Of Truth

- Beads epic: `dirtydash-refresh-loop`
- Beads loop metadata: workflow, run policy, branch/PR policy, quality gates, callback policy, actor-specific thread defaults, and implementation swarm policy
- Roadmap: `docs/implementation/refresh-remote-harness-layout-theme/00-roadmap.md`
- Loop state mirror: `docs/implementation/refresh-remote-harness-layout-theme/loop-state.md`
- Phase docs linked from Beads child issues
- Turn docs: `docs/implementation/refresh-remote-harness-layout-theme/turn-docs/`

## Loop Rules

- Select exactly one next ready Beads child issue.
- Follow the selector gate before launching implementation.
- Read the linked phase doc before editing.
- Continue phase-by-phase by default until the epic is complete, blocked, interrupted, or review/CI is unresolved.
- Use `run once` / `--once` only when intentionally running one phase and stopping after closeout.
- Keep one active implementation PR at a time.
- File Beads follow-ups instead of widening the selected phase.
- Update Beads first, then update `loop-state.md`.
- Child threads default to `speed: standard`, `reasoning: high`, and `inherit_orchestrator_thread_settings: false`.
- Implementation/review threads must be created in the intended Codex project worktree. A child that starts in the wrong repo/worktree blocks and calls back instead of self-relocating.
- Orchestrator-callback is callback-wait. After launching a worker or reviewer, wait for exactly one callback and use only sparse fallback heartbeat when overdue or liveness is uncertain.
- Worker/reviewer prompts must carry the literal orchestrator thread id. Do not launch if the prompt uses generic callback-target wording.

## Review And CI

Reviewer agents must use:

`thermo-nuclear-code-quality-review`

Reviewer and CI verification agents own CI.

Allowed CI closeout states:

- `ci-green`
- `ci-repaired-and-green`
- `ci-unavailable-with-evidence`
- `ci-blocked-with-cause`

Unknown CI is not approval.

## Turn Docs

Each phase has exactly one Markdown turn doc:

`docs/implementation/refresh-remote-harness-layout-theme/turn-docs/<phase-issue-id>.md`

Implementation, review, CI, repairs, PR state, Beads updates, follow-ups, and closeout all go into the same doc.

## Storyboard

When the epic is complete, generate:

`docs/implementation/refresh-remote-harness-layout-theme/storyboard-post-run-07-03-2026.html`

Use `impeccable` when present. If missing, continue without it and note that it was skipped.

Install `@pierre/diffs` in the target repo if missing. Every diff must use `@pierre/diffs/ssr`.

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

## Branch And PR Policy

- One active phase PR at a time.
- Implementation branches use `lavender/dirtydash-refresh-loop-<phase-slug>`.
- Branches start from `main` unless the orchestrator intentionally chooses a more current base after closeout.
- PRs target `main` on `origin`.
- Workers own their phase branch, gates, PR creation, turn doc implementation notes, and exactly one implementation callback.
- Reviewers own strict review, CI inspection, safe in-scope repairs, reruns, turn doc review/CI notes, and exactly one review callback.
- The orchestrator alone closes Beads issues, updates `loop-state.md`, selects the next phase, and performs stream closeout.

## Related Beads

- `dirtydash-9y4`: prior live session and keyboard-first idea epic
- `dirtydash-9y4.1`: prior auto-update/live session issue
- `dirtydash-fol`: prior SSH remote usage import issue
- `dirtydash-live-watcher-future`: future live watcher/SSE follow-up, not part of this epic
