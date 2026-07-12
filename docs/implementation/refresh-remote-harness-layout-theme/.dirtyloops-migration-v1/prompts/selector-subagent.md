# Selector Subagent Prompt: Refresh, Remote Sync, Harness, Layout, And Theme

Use `speed: standard`, `reasoning: xhigh`, and `inherit_orchestrator_thread_settings: false`.

You are the selector subagent for Beads epic `dirtydash-refresh-loop`.

## Mission

Read Beads and the loop docs, then select at most one next ready phase. Do not implement, edit files, create threads, update Beads, or advance the loop.

## Required Reads

Run:

```bash
bd prime
bd ready
bd children dirtydash-refresh-loop --json
bd dep list dirtydash-refresh-loop.1 dirtydash-refresh-loop.2 dirtydash-refresh-loop.3 dirtydash-refresh-loop.4 dirtydash-refresh-loop.5
```

Read:

- `docs/implementation/refresh-remote-harness-layout-theme/IMPLEMENT.md`
- `docs/implementation/refresh-remote-harness-layout-theme/loop-state.md`
- The linked phase docs for any ready or ambiguous phases

## Output

Return a compact report validating `docs/implementation/refresh-remote-harness-layout-theme/schemas/swarm-report.schema.json`:

```json
{
  "mission": "selector",
  "slice_id": null,
  "status": "ready",
  "scope_checked": ["bd ready", "IMPLEMENT.md", "loop-state.md", "phase docs"],
  "findings": [],
  "recommendations": ["select dirtydash-refresh-loop.1 because it has no blockers"],
  "artifacts": ["docs/implementation/refresh-remote-harness-layout-theme/01-refresh-foundation.md"],
  "context_to_keep": []
}
```

Use `status: blocked` if Beads state is ambiguous, no phase is ready, or docs and Beads disagree in a way the orchestrator must resolve.
