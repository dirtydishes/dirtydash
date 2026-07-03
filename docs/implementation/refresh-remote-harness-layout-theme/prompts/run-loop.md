# Run Loop: Refresh, Remote Sync, Harness, Layout, And Theme

Workflow: `orchestrator-callback`

Canonical tracker: Beads epic `dirtydash-refresh-loop`

Concrete orchestrator thread id captured for this loop: `019f2644-e698-7671-8b0e-deefbd580b77`

If this prompt is run from any other orchestrator session, replace every `019f2644-e698-7671-8b0e-deefbd580b77` occurrence with that actual thread id before launching child threads. Do not launch a worker or reviewer if the prompt text contains generic callback-target wording.

Start from:

- Beads epic: `dirtydash-refresh-loop`
- Beads loop metadata on the epic
- Implementation index: `docs/implementation/refresh-remote-harness-layout-theme/IMPLEMENT.md`
- Resume aid: `docs/implementation/refresh-remote-harness-layout-theme/loop-state.md`

## Rules

- Beads is canonical.
- Select exactly one next ready Beads child issue.
- Use the selector gate before launching implementation.
- Continue phase-by-phase by default until the epic is complete, blocked, interrupted, or review/CI is unresolved.
- Stop after one phase only when this run explicitly says `run once` or `--once`.
- Read the linked phase doc before editing.
- Keep one active implementation PR at a time.
- Use required large bounded subagent swarms.
- Reviewer agents must use `thermo-nuclear-code-quality-review`.
- Reviewer and CI verification agents own CI.
- Child threads must launch with explicit actor defaults: `speed: standard`, `reasoning: high`, and `inherit_orchestrator_thread_settings: false`.
- Implementation/review threads must be created in the intended Codex project worktree using first-class thread environment control. Worker/reviewer prompts verify repo/worktree and branch/ref; they do not repair a wrong launch environment with shell cwd changes.
- Do not inherit child-thread speed or reasoning from the current UI, recent Codex thread defaults, or the orchestrator thread.
- Update the existing Markdown turn doc.
- Update Beads first, then update `loop-state.md`.
- Do not widen the selected phase.

## Workflow Addendum

1. Run an initial selector subagent using `docs/implementation/refresh-remote-harness-layout-theme/prompts/selector-subagent.md`.
2. Validate the selector report against `docs/implementation/refresh-remote-harness-layout-theme/schemas/swarm-report.schema.json`.
3. Create one visible project-scoped implementation thread in the intended Dirtydash worktree for the selected phase. Use `docs/implementation/refresh-remote-harness-layout-theme/prompts/implementation-thread.md`, replacing phase placeholders and keeping callback target `019f2644-e698-7671-8b0e-deefbd580b77` only if this is the actual orchestrator thread id.
4. After launching implementation, wait for exactly one implementation callback. Do not actively monitor the child thread. Use at most a sparse heartbeat around 30 minutes if callback is overdue or liveness is uncertain.
5. Validate the implementation callback against `docs/implementation/refresh-remote-harness-layout-theme/schemas/implementation-callback.schema.json`.
6. Create one visible project-scoped review thread in the intended Dirtydash worktree. Use `docs/implementation/refresh-remote-harness-layout-theme/prompts/review-thread.md`, replacing phase/PR placeholders and keeping the same concrete orchestrator thread id.
7. After launching review, wait for exactly one review callback. Unknown CI is not approval.
8. Validate the review callback against `docs/implementation/refresh-remote-harness-layout-theme/schemas/review-callback.schema.json`.
9. Run a closeout-selector subagent using `docs/implementation/refresh-remote-harness-layout-theme/prompts/closeout-selector.md`.
10. Validate the closeout-selector report, update Beads first, update `loop-state.md`, close or block the phase, then either launch the next selected phase or stop in an allowed state.

## Stream Completion

When the Beads epic is complete:

1. Verify every phase has a Markdown turn doc.
2. Generate `docs/implementation/refresh-remote-harness-layout-theme/storyboard-post-run-07-03-2026.html`.
3. Use `impeccable` when present. If missing, continue and note that it was skipped.
4. Install `@pierre/diffs` in the target repo if missing, then render every diff with `@pierre/diffs/ssr`.
5. Verify the storyboard.

## Start Prompt

Run the dirtyloops orchestrator-callback loop for Beads epic `dirtydash-refresh-loop`.

Read `docs/implementation/refresh-remote-harness-layout-theme/IMPLEMENT.md` and `docs/implementation/refresh-remote-harness-layout-theme/loop-state.md`.

Run:

```bash
bd prime
bd ready
bd children dirtydash-refresh-loop --json
bd dep list dirtydash-refresh-loop.1 dirtydash-refresh-loop.2 dirtydash-refresh-loop.3 dirtydash-refresh-loop.4 dirtydash-refresh-loop.5
```

Keep the orchestrator session orchestrator-only. Do not implement product code in the orchestrator session.

The literal orchestrator thread id for callback payloads is `019f2644-e698-7671-8b0e-deefbd580b77`. If that is not the actual id of the executing orchestrator session, stop and replace it before launching any child thread. Do not launch worker or reviewer prompts that use generic callback-target wording.

Launch one selector subagent with `speed: standard`, `reasoning: high`, and `inherit_orchestrator_thread_settings: false` using `docs/implementation/refresh-remote-harness-layout-theme/prompts/selector-subagent.md`.

After selector validation, create one visible project-scoped Dirtydash implementation thread for the selected phase with `speed: standard`, `reasoning: high`, and `inherit_orchestrator_thread_settings: false`. Use `docs/implementation/refresh-remote-harness-layout-theme/prompts/implementation-thread.md`, replacing phase placeholders and using the literal orchestrator thread id above. The implementation thread owns the assigned branch/worktree, swarm-first implementation, local gates, PR, existing phase turn doc, and exactly one implementation callback.

After the implementation callback is `pr-ready`, create one visible project-scoped Dirtydash review thread with `speed: standard`, `reasoning: high`, and `inherit_orchestrator_thread_settings: false`. Use `docs/implementation/refresh-remote-harness-layout-theme/prompts/review-thread.md`, replacing phase/PR placeholders and using the same literal orchestrator thread id. The review thread must use `thermo-nuclear-code-quality-review` and owns CI, safe in-scope repairs, reruns, evidence, the existing phase turn doc, and exactly one review callback.

After each review callback, launch a closeout-selector subagent with `speed: standard`, `reasoning: high`, and `inherit_orchestrator_thread_settings: false` using `docs/implementation/refresh-remote-harness-layout-theme/prompts/closeout-selector.md`. The orchestrator alone updates Beads, updates `loop-state.md`, closes or blocks phase issues, selects the next phase, and performs stream closeout/storyboard generation.

Do not widen the selected phase. File Beads follow-ups for adjacent discoveries. Preserve the future boundary: live watcher/SSE remains `dirtydash-live-watcher-future`, not part of this epic.
