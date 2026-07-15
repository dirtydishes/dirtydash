# dirtydash

dirtydash is a local-first dashboard for inspecting AI coding usage across both terminal-native and GUI agent harnesses and tools, including Codex, T3 Code, Claude Code, OpenCode, pi, and others. It is built for developers who want to answer grounded questions about token volume, estimated cost, cache behavior, model usage, sessions, source files, and provenance without sending their activity to a hosted SaaS dashboard.

The project is intentionally practical and a little blunt: scan the files your tools already write, import the usage metadata into local SQLite, and serve a dense dashboard from your own machine. The accepted next product step keeps that loopback-local experience while adding an optional self-hosted Hub plus per-machine Collectors for metadata-only fleet sync.

## Current State And Project Roadmap

dirtydash is in a V1 foundation state. The local CLI, SQLite index, import path, bundled dashboard, pricing scaffold, and basic health checks exist, but the product is not a finished V1 release yet. Treat it as a developer preview while the remaining V1 surface is tightened.

Usage costs are estimates. dirtydash keeps provenance and confidence visible because source formats and pricing assumptions can drift.

### V1: Local Token Observatory

- [x] Rust CLI binary named `dirtydash`
- [x] Local config and SQLite database paths, with `--config` and `--db` overrides for testing or custom installs
- [x] Source scanning for Claude Code, Codex, OpenCode, and pi-agent
- [x] Metadata-only import by default
- [x] SQLite storage for usage events, source file records, pricing records, remotes, and migrations
- [x] Idempotent import keyed by raw event hash
- [x] Usage and cost aggregation by source, model, project, session, and day
- [x] Bundled pricing snapshot plus manual pricing overrides and local/free model marking
- [x] Basic doctor checks for config, database, source detection, parser health, and pricing assumptions
- [x] Read-only SSH remote discovery configuration
- [x] Embedded React/Vite dashboard served by the Rust binary
- [x] CLI tests for scan, import, doctor, pricing, serve startup, parser behavior, idempotency, and malformed records
- [x] Core pages for Overview, The Sink, Sources, Sessions, Projects, Models, Cache, Burn Report, Import/Files, Pricing, Privacy, Settings, and Doctor
- [ ] Replace numeric-only confidence with explicit exact / partial / inferred / unknown confidence labels
- [ ] Add deeper provenance drilldowns from dashboard rows to source files and parser metadata
- [ ] Add source reindex and ignore commands
- [ ] Clarify unknown, stale, unpriced, and partially inferred states in the UI
- [ ] Decide whether redacted previews belong in V1; current import is metadata-only and does not expose conversation previews
- [ ] Harden importer behavior with real-world fixtures and parser diagnostics
- [ ] Polish first-run guidance, empty states, and setup repair

### V1.1: Self-Hosted Hub And Collector Fleet

- [ ] Self-hosted Hub with canonical fleet database, dashboard, and `/api/v1` ingestion
- [ ] Outbound-only per-machine Collectors that push metadata-only usage events
- [ ] Machines and Settings product surfaces, plus the redesigned fleet Usage workspace
- [ ] Better model alias mapping and unknown-pricing warnings
- [ ] Expanded cache and reasoning token accounting
- [ ] More importer fixtures and cost regression tests
- [ ] Bundled pricing updates from external model price sources
- [ ] Reconciliation views that explain differences between dirtydash estimates and other tools

Earlier roadmap work toward agentless SSH-pull usage import is now superseded by the Hub/Collector stream preserved in `docs/implementation/refresh-remote-harness-layout-theme/` for history.

### V2: Broader Harness Support

- [ ] Importers for Gemini CLI, Pi, Hermes, Goose, Amp, Qwen, Kimi, Copilot CLI, and other coding harnesses
- [ ] Support for GUI coding agents where their local usage traces are available
- [ ] Live session tailing
- [ ] Parser diagnostics and richer Import/Files views
- [ ] Advanced Burn Report insights
- [ ] Export/import support for offline or airgapped machines

### Post-V2: Collaboration On Top Of The Fleet

- [ ] Shared dashboards and team/workspace views
- [ ] Role-based access and privacy-aware sharing
- [ ] Alerts, budgets, anomaly detection, and usage recommendations
- [ ] Plugin/importer system for custom harnesses

## Why It Exists

AI coding tools leave useful usage traces behind, but those traces are scattered across tool-specific directories and formats. dirtydash aims to turn those traces into a local instrument that helps answer:

- How many tokens did I use?
- What did that probably cost?
- Which models and projects are driving usage?
- What cache reads or writes do the logs actually report?
- Which sessions are worth inspecting?
- Which numbers are trustworthy, stale, unknown, or estimated?

The product bias is toward inspection over persuasion. It should help you verify what happened, not invent a confident story around incomplete data.

## Repository Layout

```text
.
├── crates/dirtydash/        # Rust CLI, SQLite, importers, pricing, remotes, server
├── dashboard/               # React/Vite dashboard source and built assets
├── docs/turns/              # Implementation turn records
├── PRODUCT.md               # Product positioning, users, tone, design principles
├── Cargo.toml               # Rust workspace
└── Cargo.lock
```

The Rust server embeds `dashboard/dist`, so the dashboard build is part of the shipped binary experience.

## Requirements

- Rust toolchain with Cargo
- Node.js and npm for rebuilding the dashboard
- SSH access only if using remote discovery

## Build

Build the dashboard first, then build the Rust binary:

```bash
cd dashboard
npm install
npm run build

cd ..
cargo build
```

Run tests from the repository root:

```bash
cargo test
```

## Basic Usage

Scan for known local sources:

```bash
cargo run -p dirtydash -- scan
```

Import detected or configured sources into SQLite:

```bash
cargo run -p dirtydash -- import
```

Serve the local dashboard:

```bash
cargo run -p dirtydash -- serve --open
```

By default, the server listens on `127.0.0.1:4599`.

Run health checks:

```bash
cargo run -p dirtydash -- doctor
```

Upgrade an existing dirtyloops loop to the current generated runtime artifacts:

```bash
cargo run -p dirtydash -- loop upgrade docs/implementation/my-stream
```

The command refreshes `prompts/run-loop.md`, `schemas/*.json`, and orchestrator worker/reviewer prompt files when the loop uses `orchestrator-callback`. It preserves phase docs, turn docs, `loop-state.md`, and Beads state. Use `--check` in CI or `--dry-run` before writing. If dirtydash cannot find the installed skill automatically, pass `--dirtyloops-root /path/to/skills/dirtyloops` or set `DIRTYLOOPS_ROOT`.

List pricing records:

```bash
cargo run -p dirtydash -- pricing list
```

Override a model price:

```bash
cargo run -p dirtydash -- pricing override \
  --provider openai \
  --model gpt-5.3-codex \
  --input 1.75 \
  --output 14.0
```

Mark a local or free model as zero-cost:

```bash
cargo run -p dirtydash -- pricing mark-free \
  --provider local \
  --model my-local-model
```

## Source Roots

dirtydash can detect common default locations, or you can pass explicit source roots using `kind=path` syntax:

```bash
cargo run -p dirtydash -- \
  --source-root claude-code="$HOME/.claude/projects" \
  --source-root codex="$HOME/.codex/sessions" \
  scan
```

Supported source kinds:

- `claude-code`
- `codex`
- `opencode`
- `pi-agent`

Useful global overrides:

```bash
--config /path/to/config.toml
--db /path/to/dirtydash.sqlite3
--source-root kind=/path/to/source
```

## Remote Discovery

Current shipped remote support is deliberately conservative. It stores remote definitions and can perform read-only SSH file discovery without installing an agent on the remote machine.

That existing behavior is not the active sync roadmap. The accepted roadmap is the metadata-only Hub/Collector fleet in `docs/implementation/remote-hub-collector-fleet/`, and the older SSH-pull import plan is preserved only as superseded history.

Add a remote:

```bash
cargo run -p dirtydash -- remote add workstation user@host \
  --source-root codex="~/.codex/sessions"
```

List remotes:

```bash
cargo run -p dirtydash -- remote list
```

Sync remote discovery metadata:

```bash
cargo run -p dirtydash -- remote sync workstation
```

This does not yet import remote usage events into the local database. The active roadmap replaces the old SSH-pull import direction with Hub/Collector push sync.

## Dashboard

The current dashboard includes pages for:

- Overview
- The Sink
- Sources
- Sessions
- Projects
- Models
- Cache
- Burn Report
- Import/Files
- Pricing
- Privacy
- Settings
- Doctor

The dashboard emphasizes compact inspection: totals, breakdowns, searchable sessions, parser provenance, source health, pricing records, and doctor warnings.

## Data And Privacy

dirtydash is local-first:

- The database is local SQLite.
- Import is metadata-only by default.
- Stored events include usage numbers, model/provider data, project/session identifiers, parser metadata, raw path, raw span, event hash, import time, pricing version, and confidence.
- The app does not require a hosted backend for the local experience.
- Current SSH remote behavior is pull-based discovery from the local machine.
- The accepted fleet roadmap keeps `/api/v1` and Hub persistence metadata-only as well.

The project should continue to prefer visible provenance and honest uncertainty over hidden assumptions.

## Development Notes

Useful commands:

```bash
cargo test
cargo run -p dirtydash -- scan --json
cargo run -p dirtydash -- import --json
cargo run -p dirtydash -- doctor --json
```

Dashboard development:

```bash
cd dashboard
npm run dev
```

Production dashboard assets:

```bash
cd dashboard
npm run build
```

Because the Rust server embeds the built dashboard assets, rebuild `dashboard/dist` before expecting frontend changes to appear in the binary-served app.

## Project Posture

dirtydash should feel technical, calm, dense, trustworthy, and terminal-native. The UI and docs should avoid fake precision, generic AI SaaS language, growth-dashboard energy, and anything that hides uncertainty. The best version of this project is a compact local workbench that lets developers inspect their own AI coding activity with confidence.
