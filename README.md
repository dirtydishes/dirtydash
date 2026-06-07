# dirtydash

dirtydash is a local-first dashboard for inspecting AI coding usage across terminal-native tools. It is built for developers who want to answer grounded questions about token volume, estimated cost, cache behavior, model usage, sessions, source files, and provenance without sending their activity to a hosted SaaS dashboard.

The project is intentionally practical and a little blunt: scan the files your tools already write, import the usage metadata into local SQLite, and serve a dense dashboard from your own machine.

## Current State

dirtydash is in an early V1 foundation state. The core workflow exists, but the product is still young and should be treated as a developer preview rather than polished release software.

Implemented today:

- Rust CLI binary named `dirtydash`
- Local config and SQLite database paths, with `--config` and `--db` overrides for testing or custom installs
- Source scanning for Claude Code, Codex, OpenCode, and pi-agent
- Metadata-only import by default
- SQLite storage for usage events, source file records, pricing records, remotes, and migrations
- Idempotent import keyed by raw event hash
- Usage and cost aggregation by source, model, project, session, and day
- Bundled pricing snapshot plus manual pricing overrides and local/free model marking
- Basic doctor checks for config, database, source detection, parser health, and pricing assumptions
- Read-only SSH remote discovery configuration
- Embedded React/Vite dashboard served by the Rust binary
- CLI tests for scan, import, doctor, pricing, serve startup, parser behavior, idempotency, and malformed records

Important current limitations:

- Remote support currently discovers files over SSH; it is not yet a full remote import/sync pipeline.
- Pricing is a bundled snapshot plus local overrides, not a live pricing service.
- The dashboard is useful but still mostly inspection-oriented; many settings are CLI-backed rather than editable in the UI.
- Importers are real, but still need more real-world fixtures before they should be considered hardened.
- Usage costs are estimates. dirtydash keeps provenance and confidence visible because source formats and pricing assumptions can drift.

## Why It Exists

AI coding tools leave useful usage traces behind, but those traces are scattered across tool-specific directories and formats. dirtydash aims to turn those traces into a local instrument that helps answer:

- How many tokens did I use?
- What did that probably cost?
- Which models and projects are driving usage?
- How much cache is being read or written?
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

V1 remote support is deliberately conservative. It stores remote definitions and can perform read-only SSH file discovery without installing an agent on the remote machine.

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

This does not yet import remote usage events into the local database. That is part of the roadmap.

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
- The app does not require a hosted backend.
- SSH remote behavior is pull-based discovery from the local machine.

The project should continue to prefer visible provenance and honest uncertainty over hidden assumptions.

## Roadmap

### V1: Local Foundation

V1 is slightly adjusted to match the current codebase: the first version is not a giant end-to-end ingestion machine yet. It is a working local foundation with a real CLI, SQLite schema, import pipeline, bundled dashboard, and cautious remote discovery.

Already present:

- Local source scanning
- Metadata-only import
- SQLite persistence and migrations
- Cost estimation using bundled pricing and overrides
- Dashboard server and embedded UI
- Doctor checks
- Initial remote configuration and SSH discovery
- Test coverage for the core happy path

Still needed for V1:

- Harden importers with real-world fixtures from supported tools
- Improve Codex model/pricing coverage as model names evolve
- Turn SSH discovery into a safe remote import workflow
- Clarify stale, unknown, and unpriced states in the dashboard
- Add more explicit first-run guidance
- Make UI empty states and error states more useful
- Package a repeatable install/release path

### V2: Trust And Inspection Depth

Planned after the foundation is stable:

- Better parser diagnostics and per-file import reports
- Session drill-downs with richer provenance
- Time range controls and comparisons
- More cache analysis, including savings estimates and cache miss patterns
- Better model alias handling
- Safer handling for changed upstream log formats
- Exportable reports for local review

### V3: Multi-Machine Workflow

Longer-term direction:

- Full pull-based remote import across machines
- Remote source health summaries
- Machine-level comparisons
- Deduplication across copied or synced session files
- Optional scheduled imports
- Stronger controls for what metadata is stored locally

### V4: Product Polish

Once the data path is trustworthy:

- More complete dashboard interactions
- Keyboard-first navigation
- Saved filters and views
- Better onboarding and setup repair
- Installer or packaged binary distribution
- Documentation for common tool setups

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
