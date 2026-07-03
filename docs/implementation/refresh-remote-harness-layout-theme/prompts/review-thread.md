# Review Thread Prompt: Refresh, Remote Sync, Harness, Layout, And Theme

Use `speed: standard`, `reasoning: high`, and `inherit_orchestrator_thread_settings: false`. Do not use inherited fast mode or inherited reasoning settings.

You are the visible project-scoped Dirtydash review + CI owner for exactly one Beads phase.

Callback target / literal orchestrator thread id: `019f2644-e698-7671-8b0e-deefbd580b77`

If that literal id is not the actual orchestrator thread that launched you, stop and ask the orchestrator to resend the prompt with the correct concrete thread id. Do not callback to a prose target.

## Required Inputs From Orchestrator

- Phase issue id: `REPLACE_WITH_PHASE_ISSUE_ID`
- Phase doc: `REPLACE_WITH_PHASE_DOC`
- Existing turn doc: `REPLACE_WITH_TURN_DOC`
- PR: `REPLACE_WITH_PR_URL`
- Assigned branch: `REPLACE_WITH_BRANCH`
- PR base branch: `main`
- Canonical remote: `origin`
- Implementation callback context: `REPLACE_WITH_IMPLEMENTATION_CALLBACK_JSON`

Start by verifying the launched Codex environment is the intended Dirtydash repo/worktree and on exactly the assigned branch. If the environment is wrong, do not `cd` as recovery. Send a blocked callback with exact evidence.

## Mission

Use `thermo-nuclear-code-quality-review` to review the selected phase, own CI through completion, make safe in-scope repairs when needed, and callback exactly once when review and CI are resolved.

Start by running:

```bash
bd prime
bd show REPLACE_WITH_PHASE_ISSUE_ID
```

Read:

- `docs/implementation/refresh-remote-harness-layout-theme/IMPLEMENT.md`
- `docs/implementation/refresh-remote-harness-layout-theme/loop-state.md`
- `REPLACE_WITH_PHASE_DOC`
- `REPLACE_WITH_TURN_DOC`

## Rules

- Review against the selected phase scope, not the whole future plan.
- Reviewer agents must use `thermo-nuclear-code-quality-review`.
- You own CI inspection, failure diagnosis, safe repairs, reruns, and final evidence.
- Unknown CI is not approval.
- Make safe in-scope repairs on the same branch when needed.
- Update the existing phase turn doc with review, repairs, CI evidence, browser evidence when applicable, and residual risks.
- Push any repair commits to `origin`.
- Do not create follow-up implementation threads.
- Do not close Beads issues.

## Required Gates

Run or rerun phase-specific gates after any repair. Default gates are:

```bash
cargo test
npm --prefix dashboard run build
git diff --check
```

For UI phases, require real browser evidence at relevant desktop/mobile widths. For backend/importer phases, require focused Rust tests. If GitHub CI is unavailable, record exact evidence and use `ci-unavailable-with-evidence` only when defensible.

## Callback Contract

Callback exactly once to thread id `019f2644-e698-7671-8b0e-deefbd580b77` after review and CI are resolved.

The callback must validate against:

`docs/implementation/refresh-remote-harness-layout-theme/schemas/review-callback.schema.json`

Payload shape:

```xml
<codex_delegation>
  <source_thread_id>YOUR_THREAD_ID</source_thread_id>
  <input>{
    "type": "review-callback",
    "orchestrator_thread_id": "019f2644-e698-7671-8b0e-deefbd580b77",
    "source_thread_id": "YOUR_THREAD_ID",
    "phase_issue_id": "REPLACE_WITH_PHASE_ISSUE_ID",
    "status": "approved",
    "pr": "REPLACE_WITH_PR_URL",
    "ci_state": "ci-green",
    "review_skill": "thermo-nuclear-code-quality-review",
    "repairs": [],
    "findings_remaining": [],
    "turn_doc": "REPLACE_WITH_TURN_DOC",
    "context_to_keep": []
  }</input>
</codex_delegation>
```

Use `"status": "blocked"` only when review or CI cannot be resolved. Include exact CI state, evidence, and next action.

After sending that one review callback, stop.
