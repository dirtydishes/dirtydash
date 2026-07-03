# Phase 2: Ledger Layout Reshape

Canonical Beads issue: `dirtydash-refresh-loop.2`

Epic: `dirtydash-refresh-loop`

Status is tracked in Beads. This doc is implementation context.

## Outcome

Reshape the Ledger workspace so the chart is the primary full-width top module, daily usage and inspector sit below it, and selected-day sessions are removed from Ledger while the global Sessions workspace remains intact.

## Scope

Allowed:

- Make the chart the primary full-width top module.
- Make the chart larger horizontally and slightly taller vertically.
- Place daily usage and inspector below the chart.
- Remove selected-day sessions from the Ledger workspace.
- Keep the global Sessions workspace.
- Update keyboard focus order so Ledger includes only chart, daily usage, and inspector.
- Remove selected-day session state/effects from the React app.
- Remove `/api/days/:day/sessions` only if no other UI or test still uses it.
- Preserve compact, terminal-native density and fixed-viewport behavior.

Out of scope:

- Refresh manager changes beyond preserving Phase 1 status/control.
- Theme token sets.
- Remote sync or harness import work.
- New session detail product surfaces.
- Broad redesign of non-Ledger workspaces.

## Inputs

- Phase 1 closeout and turn doc
- Dashboard workspace state: `dashboard/src/main.tsx`
- Dashboard layout styles: `dashboard/src/styles.css`
- Server day-session route, if removal is safe: `crates/dirtydash/src/server.rs`
- Existing Sessions workspace behavior in `dashboard/src/main.tsx`

## Implementation Notes

- Keep focus order predictable for repeated keyboard use.
- If `/api/days/:day/sessions` has any remaining consumer or test, leave it and note the reason.
- Use stable dimensions so chart, rows, and inspector do not shift under hover or data refresh.

## Beads

- Epic: `dirtydash-refresh-loop`
- Issue: `dirtydash-refresh-loop.2`
- Depends on: `dirtydash-refresh-loop.1`
- Parallel-safe: `false`

## Expected Files Or Areas

- `dashboard/src/main.tsx`
- `dashboard/src/styles.css`
- `crates/dirtydash/src/server.rs` only if route removal is safe
- `crates/dirtydash/tests/cli.rs` only if server/API behavior changes

## Suggested Swarms

- 8-20 scout agents across current Ledger state, keyboard navigation, selected-day sessions usage, and responsive constraints.
- 8-16 slice-plan agents for component state cleanup, CSS layout, optional API removal, and tests/smoke.
- 8-16 implementation-helper agents for bounded UI and cleanup slices.

## Quality Gates

- `npm --prefix dashboard run build`
- `cargo test` if API/server code changes
- Browser responsive smoke at desktop, medium, and mobile widths
- Keyboard focus-order check
- `git diff --check`

## Completion Criteria

- Chart is the full-width top Ledger module and has more visual priority.
- Daily usage and inspector are below the chart.
- Selected-day sessions are removed from Ledger.
- Global Sessions workspace still works.
- Ledger focus order only includes chart, daily usage, and inspector.
- Unused selected-day session state/effects are removed.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
