# Phase 6: Usage Redesign and Themes

Canonical Beads issue: `dirtydash-px3.6`

Epic: `dirtydash-px3`

Status is tracked in Beads. This document preserves accepted intent and is decision-complete, implementation-open.

## Outcome

The dashboard becomes a keyboard- and touch-accessible fleet ledger organized around Usage, Machines, and Settings, with stable live interaction and a semantic, persistent theme system.

## Why This Phase Exists

The old workspace rail and session-oriented layout do not express fleet-wide daily usage, machine health, or self-hosted administration.

## Scope

Allowed:

- Usage, Machines, and Settings information architecture.
- `1D`, `1W`, `1M`, `3M`, custom range; Agents, Machines, Projects, Models; daily stacked rows; token and estimated-cost columns; toggled legends; detailed segment inspection; chronological/token/cost sort; absolute/log scale.
- Session inspector opened from a day, without restoring a permanent session workspace.
- `r`, `/`, Escape, arrow navigation, stable selection during live batches, animated/static sync status, and phone reductions.
- Dirtydash Mono, Matrix, retained Catppuccin/Tokyo Night/Dracula/Gruvbox/Nord/Rose Pine/One Dark/Solarized catalog, semantic OKLCH tokens, persistence, and WCAG AA.

Out of scope:

- Mobile enrollment or destructive administration.
- Matrix code rain or ambient animation.

## Constraints

- `1M` and Agents are defaults.
- Every control has default, hover, focus, active, disabled, loading, and error states.
- Data/status remains distinguishable without hue alone.
- Existing data renders immediately while synchronization proceeds.

## Settled Decisions

Dirtydash Mono is default; color is reserved for data and status. Reduced motion uses static `syncing`. Agents, projects, and models are discovered.

## Open Questions

None.

## Dependencies

- Depends on: `dirtydash-px3.5`
- Parallel-safe: no.

## Acceptance Evidence

- Frontend tests cover range, breakdown, sort, scale, selection stability, empty/error/stale states, inspector behavior, and all interaction states.
- Browser tests cover keyboard and touch behavior, reduced motion, desktop/tablet/phone reductions, every theme, and contrast.

## Quality Gates

- `npm --prefix dashboard run build`
- `cargo test` for API/server changes.
- Browser and accessibility smoke at desktop, tablet, and phone widths.
- Automated and manual WCAG AA/theme checks.

## Replanning Triggers

Replan if the stacked daily ledger cannot remain keyboard/touch accessible at accepted density or semantic tokens cannot meet WCAG AA without changing accepted hierarchy.

## Implementation Hypotheses

These are suggestions to validate against repository evidence, not mandatory choreography.

- Preserve interaction state by stable domain identifiers rather than row indexes.
- Keep theme differences in semantic tokens rather than component-specific overrides.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
