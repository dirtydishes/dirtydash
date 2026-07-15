# Refresh, Remote Sync, Harness, Layout, And Theme Roadmap

> **Superseded roadmap — stop:** preserved for historical context only. Do not schedule or launch work from this plan. Active remote architecture planning moved to `docs/implementation/remote-hub-collector-fleet/` under Beads epic `dirtydash-px3`.

Canonical tracker: Beads epic `dirtydash-refresh-loop`

## Plan Source

Conversation attachment `PLAN (13).md`, normalized into this Beads-canonical dirtyloop. Generated loop artifacts use repo-relative paths only.

## Outcome

Historical outcome only: Dirtydash gets one explicit refresh path used by dashboard launch, refresh button, and `r`; the Ledger workspace is reshaped around a larger chart; built-in themes are selectable and persisted; remote sync imports usage metadata agentlessly over SSH; and OpenCode/Hermes Agent support is hardened with fixtures. The active remote direction now supersedes the SSH-pull portion with the Hub/Collector fleet. Live watcher/SSE remains future work.

## Phase Sequence

1. `dirtydash-refresh-loop.1` - Refresh Foundation
2. `dirtydash-refresh-loop.2` - Ledger Layout Reshape
3. `dirtydash-refresh-loop.3` - Built-In Themes
4. `dirtydash-refresh-loop.4` - Agentless SSH Remote Sync
5. `dirtydash-refresh-loop.5` - OpenCode And Hermes Agent Harness Support

## Dependencies

The stream is intentionally serialized to keep one active implementation PR at a time:

- Phase 2 depends on Phase 1.
- Phase 3 depends on Phase 2.
- Phase 4 depends on Phase 3.
- Phase 5 depends on Phase 4.

Remote sync is after explicit refresh so remote work can be triggered in the background without blocking local freshness. Harness support follows remote sync so OpenCode/Hermes fixture work can verify both local refresh and remote import paths.

## Risks

- Refresh must not block initial dashboard rendering or duplicate imports under concurrent launch/manual requests.
- Ledger reshaping must remove selected-day sessions without breaking the global Sessions workspace.
- Theme tokens must keep chart hover/active states visible without relying only on hue.
- Remote sync must preserve remote provenance and must not keep raw mirrored logs longer than needed after successful import.
- Hermes support must remain metadata-only and must not import message content.
- Live watcher/SSE is tempting adjacent scope; keep it filed as `dirtydash-live-watcher-future`.

## Quality Gates

- `cargo test`
- `npm --prefix dashboard run build`
- Focused importer/API tests for backend phases
- Browser smoke for refresh, layout, theme, and remote status UI changes
- `git diff --check`

## Closeout

The final closeout artifact is:

`docs/implementation/refresh-remote-harness-layout-theme/storyboard-post-run-07-03-2026.html`
