# Phase 5: OpenCode And Hermes Agent Harness Support

> **Superseded phase record — stop:** preserved for historical context only. Do not launch, claim, or implement work from this doc. Active remote planning and execution live in `docs/implementation/remote-hub-collector-fleet/`.

Canonical Beads issue: `dirtydash-refresh-loop.5`

Epic: `dirtydash-refresh-loop`

Status is tracked in Beads. This doc preserves historical implementation context only.

## Outcome

Historical outcome only: harden OpenCode support with real fixtures and add metadata-only Hermes Agent support through local refresh and remote sync.

## Scope

Allowed:

- Treat existing OpenCode support as present.
- Add real OpenCode fixture coverage.
- Ensure OpenCode works through local refresh and remote sync.
- Keep `opencode` source-root compatibility.
- Add `SourceKind::HermesAgent`.
- Accept source aliases `hermes` and `hermes-agent`.
- Add default Hermes discovery candidates:
  - `~/.hermes/state.db`
  - `~/.hermes/sessions/*.jsonl`
  - `~/.hermes/webui/sessions/_run_journal/**/*.jsonl`
  - `~/.hermes/webui/sessions/_turn_journal/*.jsonl`
- Prefer Hermes `state.db` sessions table when available because it has aggregate token and cost fields.
- Add fallback parser fixtures for Hermes session JSONL and webui journal metering events.
- Read Hermes SQLite session rows into Dirtydash usage events with provider/model/cost/token fields where available.
- Preserve metadata-only behavior. Do not import message content.

Out of scope:

- Gemini, Goose, Amp, Qwen, Kimi, Copilot CLI, or any harness beyond OpenCode and Hermes Agent.
- Hosted sync or server-side sharing.
- Remote daemon installation.
- Redacted content previews.

## Inputs

- Phase 4 remote sync closeout and turn doc
- Source kind model and parsers: `crates/dirtydash/src/importers.rs`
- Config/source-root parsing: `crates/dirtydash/src/config.rs`
- Database event write path: `crates/dirtydash/src/db.rs`
- CLI tests and importer fixtures: `crates/dirtydash/tests/cli.rs`

## Implementation Notes

- Keep source aliases explicit and tested.
- Ensure Hermes state DB parser gracefully degrades to JSONL/journal fallback.
- Reported Hermes cost fields can be imported as reported cost when available, but pricing behavior should remain explainable.
- Make fixture coverage representative enough to catch parser drift.

## Beads

- Epic: `dirtydash-refresh-loop`
- Issue: `dirtydash-refresh-loop.5`
- Depends on: `dirtydash-refresh-loop.4`
- Parallel-safe: `false`

## Expected Files Or Areas

- `crates/dirtydash/src/importers.rs`
- `crates/dirtydash/src/config.rs`
- `crates/dirtydash/src/db.rs`
- `crates/dirtydash/tests/cli.rs`
- Test fixture directories/files as needed
- Dashboard source labels only if UI/source presentation needs updates

## Suggested Swarms

- 8-20 scout agents across OpenCode fixture gaps, Hermes data shapes, SQLite parsing, JSONL/journal parsing, pricing/confidence, and remote provenance.
- 8-16 slice-plan agents for SourceKind plumbing, Hermes state DB parser, JSONL fallback parser, OpenCode fixtures, remote refresh integration, and tests.
- 8-16 implementation-helper agents for bounded parser/test slices.

## Quality Gates

- `cargo test`
- Focused Hermes state DB and journal fixture tests
- Focused OpenCode local refresh and remote sync fixture tests
- `npm --prefix dashboard run build` if UI/source labels change
- `git diff --check`

## Completion Criteria

- OpenCode has real fixture coverage and works through local refresh and remote sync.
- `SourceKind::HermesAgent` exists with `hermes` and `hermes-agent` aliases.
- Hermes default discovery covers the planned state DB and JSONL/journal paths.
- Hermes state DB sessions table is preferred when available.
- Hermes JSONL/journal fallback fixtures pass.
- Hermes imports are metadata-only and do not import message content.
- No extra harness families are added.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
