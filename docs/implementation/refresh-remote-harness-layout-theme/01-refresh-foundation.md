# Phase 1: Refresh Foundation

Canonical Beads issue: `dirtydash-refresh-loop.1`

Epic: `dirtydash-refresh-loop`

Status is tracked in Beads. This doc is implementation context.

## Outcome

Create one refresh path that is used by dashboard launch, refresh button, and `r`, backed by a server-side single-flight refresh manager behind `POST /api/refresh`.

## Scope

Allowed:

- Add a server-side refresh manager behind `POST /api/refresh`.
- Keep dashboard launch fast by loading existing SQLite data first, then starting refresh in the background.
- Make refresh button and `r` trigger the same refresh path.
- Exclude typing targets from the `r` shortcut.
- Seed pricing before local metadata-only import, matching current CLI import behavior.
- Join concurrent launch/manual requests onto the same running refresh job.
- Return refresh status/report: `idle`, `running`, `succeeded`, or `failed`; inserted/updated/skipped counts; parse errors; and started/finished timestamps.
- Add subdued command/status-area UI for `syncing`, `+N events`, `synced HH:MM`, or warning.
- Keep current day/session selection stable when new data arrives.
- Add focused tests for single-flight behavior and API/report shape.

Out of scope:

- Agentless remote import. Phase 4 owns remote sync.
- Live filesystem watcher, SSE, or automatic file tailing. That is `dirtydash-live-watcher-future`.
- Ledger layout reshaping beyond what is needed to place refresh status/control.
- Theme selection.
- Conversation/output preview import.

## Inputs

- Plan source: `docs/implementation/refresh-remote-harness-layout-theme/00-roadmap.md`
- Current server API: `crates/dirtydash/src/server.rs`
- Current CLI import behavior: `crates/dirtydash/src/cli.rs`
- Import/pricing APIs: `crates/dirtydash/src/importers.rs`, `crates/dirtydash/src/pricing.rs`
- Dashboard load and keyboard handling: `dashboard/src/main.tsx`
- Dashboard styles: `dashboard/src/styles.css`

## Implementation Notes

- Prefer a shared application refresh service over duplicating CLI-only import logic inside handlers.
- The launch path should return existing API data without waiting for import.
- The refresh report should be durable enough for UI and tests, but do not create a background daemon or watcher.
- If database concurrency gets tricky, keep the manager boundary explicit and test single-flight behavior directly.

## Beads

- Epic: `dirtydash-refresh-loop`
- Issue: `dirtydash-refresh-loop.1`
- Depends on: none
- Parallel-safe: `false`

## Expected Files Or Areas

- `crates/dirtydash/src/server.rs`
- `crates/dirtydash/src/cli.rs`
- `crates/dirtydash/src/importers.rs`
- `crates/dirtydash/src/pricing.rs`
- `crates/dirtydash/src/db.rs`
- `crates/dirtydash/tests/cli.rs`
- `dashboard/src/main.tsx`
- `dashboard/src/styles.css`

## Suggested Swarms

- 8-20 scout agents across server state, import/pricing reuse, dashboard load, keyboard handling, and test seams.
- 8-16 slice-plan agents for refresh manager, API contract, UI refresh/status, and focused tests.
- 8-16 implementation-helper agents for bounded slices.

## Quality Gates

- `cargo test`
- `npm --prefix dashboard run build`
- Browser smoke for launch background refresh, manual refresh button, `r` shortcut, subdued status, and selection stability
- `git diff --check`

## Completion Criteria

- `POST /api/refresh` exists and uses a server-side single-flight manager.
- Launch loads existing SQLite data immediately and starts refresh in the background.
- Manual refresh and `r` use the same path.
- Refresh status/report includes state, counts, parse errors, and timestamps.
- Selection remains stable when refreshed data arrives.
- Tests or smoke evidence cover the selected refresh behavior.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
