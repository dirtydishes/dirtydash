# Phase 4 Turn Doc: Agentless SSH Remote Sync

> **Superseded turn doc:** preserved for historical context only. Active remote implementation moved to `dirtydash-px3` and the Hub/Collector fleet stream.

Beads issue: `dirtydash-refresh-loop.4`

Phase doc: `docs/implementation/refresh-remote-harness-layout-theme/04-agentless-ssh-remote-sync.md`

This is the single Markdown turn doc for the phase.

## Phase Selection

Not started in this historical stream. The remote direction was later superseded by `dirtydash-px3`.

## Scope

See the phase doc. Keep this phase to agentless remote usage import and manifest/provenance integration.

## Implementation Log

Not started.

## Subagent Swarms

Required for non-trivial implementation: scout, slice-plan, and implementation-helper swarms before broad edits.

## Review

Reviewer skill:

`thermo-nuclear-code-quality-review`

Not started.

## CI And Gates

CI owner: reviewer/verification agents

Current CI state: `ci-blocked-with-cause`

Evidence:

- Phase has not started.

## PR And Commits

None.

## Beads Updates

Loop scaffold created.

## Follow-Ups Filed

None yet.

## Context To Keep

- Keep remote sync agentless: no daemon and no inbound service.
- Remote failures must not block local dashboard freshness.
- Raw remote mirrors are temporary; SQLite rows and the manifest are durable.

## Closeout

Open.
