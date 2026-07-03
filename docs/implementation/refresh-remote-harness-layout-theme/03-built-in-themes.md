# Phase 3: Built-In Themes

Canonical Beads issue: `dirtydash-refresh-loop.3`

Epic: `dirtydash-refresh-loop`

Status is tracked in Beads. This doc is implementation context.

## Outcome

Add built-in themes as CSS token sets, persist the selected theme in browser `localStorage`, and expose theme selection in Ops Settings for the first pass.

## Scope

Allowed:

- Add token sets for Dirtydash default, Catppuccin Latte/Frappe/Macchiato/Mocha, Tokyo Night, Dracula, Gruvbox Dark/Light, Nord, Rose Pine/Rose Pine Moon/Rose Pine Dawn, One Dark, and Solarized Dark/Light.
- Implement themes as CSS theme token sets, not separate stylesheets.
- Persist selected theme in browser `localStorage`.
- Put theme selection in Ops Settings only.
- Apply theme via a stable root attribute such as `data-theme`.
- Keep semantic tokens stable: background, rail, pane layers, line, ink, muted, soft, accent, success, warning, danger, and chart series.
- Verify contrast and chart active/hover states for every built-in theme.

Out of scope:

- User-defined custom themes.
- Server-side theme persistence.
- Full settings redesign.
- Palette changes unrelated to theme support.
- Remote sync or importer work.

## Inputs

- Phase 2 layout closeout and turn doc
- UI state and Ops workspace: `dashboard/src/main.tsx`
- Theme tokens/styles: `dashboard/src/styles.css`

## Implementation Notes

- Keep theme names and ids stable.
- Avoid one-note palettes; each theme can follow its source palette but should keep Dirtydash's dense operational feel.
- Ensure chart active/hover states remain distinguishable without relying only on hue.

## Beads

- Epic: `dirtydash-refresh-loop`
- Issue: `dirtydash-refresh-loop.3`
- Depends on: `dirtydash-refresh-loop.2`
- Parallel-safe: `false`

## Expected Files Or Areas

- `dashboard/src/main.tsx`
- `dashboard/src/styles.css`
- Optional focused frontend helper modules if the worker chooses to extract theme definitions

## Suggested Swarms

- 8-20 scout agents across token usage, chart colors, Ops Settings, localStorage, and accessibility risk.
- 8-16 slice-plan agents for theme registry, root attribute wiring, settings UI, CSS token sets, and verification.
- 8-16 implementation-helper agents for bounded token/UI/test slices.

## Quality Gates

- `npm --prefix dashboard run build`
- Browser smoke for theme switching and `localStorage` persistence
- Theme contrast and chart visibility spot-checks
- `git diff --check`

## Completion Criteria

- Every listed built-in theme exists as tokens.
- Theme selection is available in Ops Settings.
- Selection persists in `localStorage`.
- The root attribute applies themes.
- Contrast and chart states remain visible across themes.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
