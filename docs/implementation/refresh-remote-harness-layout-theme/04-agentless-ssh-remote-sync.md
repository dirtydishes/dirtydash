# Phase 4: Agentless SSH Remote Sync

> **Superseded phase record — stop:** preserved for historical context only. Do not launch, claim, or implement work from this doc. The accepted Hub/Collector fleet in `docs/implementation/remote-hub-collector-fleet/` replaces this agentless SSH-pull direction.

> **Superseded phase record:** preserved for historical context only. The active remote implementation stream is the Hub/Collector fleet in `docs/implementation/remote-hub-collector-fleet/`.

Canonical Beads issue: `dirtydash-refresh-loop.4`

Epic: `dirtydash-refresh-loop`

Status is tracked in Beads. This doc preserves historical implementation context only.

## Outcome

Historical outcome only: extend remote sync from file-count discovery to agentless remote usage import while preserving local-first behavior, remote provenance, and non-blocking dashboard freshness.

## Scope

Allowed:

- Extend existing `remote sync` from file-count discovery to usage import for supported source kinds.
- Keep topology agentless: Dirtydash SSHes into configured remotes, with no persistent remote daemon and no inbound remote service.
- Make launch/manual refresh run local import immediately and start configured remote sync in the background.
- Ensure remote failures do not block local dashboard freshness.
- Add a durable remote-file manifest in SQLite with remote name, source kind, remote path, size, mtime, optional content hash when needed, imported_at, and last status/error.
- Copy raw remote files to a local staging/mirror area only when needed.
- Delete raw mirrored files after successful import.
- Preserve remote machine/source identity on imported events.
- Make `raw_path` represent the remote origin, not temporary staging.
- Surface remote last success/error in source/ops areas.
- Wire source-kind acceptance for existing kinds and the planned `hermes` / `hermes-agent` aliases, with Hermes-specific parser behavior completed in Phase 5.

Out of scope:

- Persistent remote daemon.
- Inbound remote service.
- Live watcher/SSE.
- Importing message content.
- Adding non-OpenCode/non-Hermes harness families.
- Full Hermes parser fixture coverage, which Phase 5 owns.

## Inputs

- Phase 1 refresh manager closeout and turn doc
- Current remote discovery: `crates/dirtydash/src/remote.rs`
- Current config model: `crates/dirtydash/src/config.rs`
- Database migrations and summaries: `crates/dirtydash/src/db.rs`
- Import source model: `crates/dirtydash/src/importers.rs`
- Refresh API and dashboard status surfaces from earlier phases

## Implementation Notes

- Use non-interactive SSH behavior such as `BatchMode=yes`.
- Keep local import first and remote work backgrounded.
- Treat the manifest as durable import state; raw mirrored files are temporary.
- Prefer testable internal functions for remote manifest diffing and provenance rewriting.

## Beads

- Epic: `dirtydash-refresh-loop`
- Issue: `dirtydash-refresh-loop.4`
- Depends on: `dirtydash-refresh-loop.3`
- Parallel-safe: `false`

## Expected Files Or Areas

- `crates/dirtydash/src/remote.rs`
- `crates/dirtydash/src/config.rs`
- `crates/dirtydash/src/db.rs`
- `crates/dirtydash/src/importers.rs`
- `crates/dirtydash/src/server.rs`
- `crates/dirtydash/tests/cli.rs`
- `dashboard/src/main.tsx`
- `dashboard/src/styles.css`

## Suggested Swarms

- 8-20 scout agents across SSH execution, staging, manifest schema, import idempotency, provenance, refresh integration, and UI status.
- 8-16 slice-plan agents for manifest migration, remote fetch/import, refresh integration, source/ops UI, and tests.
- 8-16 implementation-helper agents for bounded backend/UI/test slices.

## Quality Gates

- `cargo test`
- Focused remote manifest/import tests
- `npm --prefix dashboard run build` if UI changes
- Browser smoke for remote status surfaces if UI changes
- `git diff --check`

## Completion Criteria

- Remote sync imports usage rows instead of only counting files for supported source kinds.
- Launch/manual refresh starts remote sync in the background after local import begins or completes.
- Remote failures do not block local freshness.
- Durable manifest records remote file state and status/error.
- Usage rows preserve remote provenance.
- `raw_path` points to the remote origin.
- Temporary raw mirrors are removed after successful import when staging is used.

## Follow-Up Policy

Do not widen this phase. File Beads follow-ups for adjacent discoveries.
