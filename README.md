# dirtydash

dirtydash is a local-first dashboard for inspecting AI coding usage across both terminal-native and GUI agent harnesses and tools, including Codex, T3 Code, Claude Code, OpenCode, pi, and others. It is built for developers who want to answer grounded questions about token volume, estimated cost, cache behavior, model usage, sessions, source files, and provenance without sending their activity to a hosted SaaS dashboard.

The project is intentionally practical and a little blunt: scan the files your tools already write, import the usage metadata into local SQLite, and serve a dense dashboard from your own machine.

## Current State

dirtydash is in a V1 foundation state. That means the local CLI, SQLite index, import path, bundled dashboard, pricing scaffold, and basic health checks exist, but the product is not a finished V1 release yet. Treat it as a developer preview while the remaining V1 surface is tightened.

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

Current limitations are tracked directly in the roadmap below. The important ones today:

- Remote support currently discovers files over SSH; it is not yet a full remote import/sync pipeline.
- Pricing is a bundled snapshot plus local overrides, not a live pricing service.
- The dashboard is useful but still mostly inspection-oriented; many settings are CLI-backed rather than editable in the UI.
- Importers are real, but still need more real-world fixtures before they should be considered hardened.
- Confidence is currently numeric in stored events and UI-adjacent data; the planned exact / partial / inferred / unknown labels still need to be made explicit.
- Source reindex/ignore commands are planned but not implemented yet.
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

## Project Roadmap

### V1: Local Token Observatory

Status: in progress. The repository has the V1 foundation, but V1 should not be called complete until confidence labels, provenance drilldowns, source management, importer hardening, and the core dashboard surface are all tightened.

Already present:

- Rust CLI and local web dashboard under the `dirtydash` binary
- SQLite-backed local import and index
- Importers for Claude Code, Codex, OpenCode, and pi-agent
- Pricing snapshot with manual overrides and local/free model marking
- Metadata-only import by default
- Stored provenance including raw path, raw span, parser name, parser version, pricing version, event hash, and import time
- Core pages for Overview, The Sink, Sessions, Sources, Cache, Burn Report, Import/Files, Pricing, Privacy, Settings, and Doctor
- CLI commands for `doctor`, `scan`, `import`, `serve`, `remote`, and `pricing`
- Embedded React/Vite dashboard served by Rust

Still needed before V1 is complete:

- Replace numeric-only confidence with explicit exact / partial / inferred / unknown confidence labels
- Add deeper provenance drilldowns from dashboard rows to source files and parser metadata
- Add source reindex and ignore commands
- Clarify unknown, stale, unpriced, and partially inferred states in the UI
- Decide whether redacted previews belong in V1; current import is metadata-only and does not expose conversation previews
- Harden importer behavior with real-world fixtures and parser diagnostics
- Polish first-run guidance, empty states, and setup repair

### V1.1: Accuracy And Remote Pull

Planned next after the V1 local surface is trustworthy:

- SSH pull-based remote machine sync that imports remote usage, not just file counts
- Machines and Remotes dashboard pages
- Better model alias mapping and unknown-pricing warnings
- Expanded cache and reasoning token accounting
- More importer fixtures and cost regression tests
- Bundled pricing updates from external model price sources
- Reconciliation views that explain differences between dirtydash estimates and other tools

### V2: Broader Harness Support

Planned once the core parser and pricing model are steady:

- Importers for Gemini CLI, Pi, Hermes, Goose, Amp, Qwen, Kimi, Copilot CLI, and other coding harnesses
- Support for GUI coding agents where their local usage traces are available
- Live session tailing
- Parser diagnostics and richer Import/Files views
- Advanced Burn Report insights
- Export/import support for offline or airgapped machines

### Post-V2: Fleet And Collaboration

Longer-term direction:

- Optional `dirtydash-agent` for remote hosts
- Shared dashboards and team/workspace views
- Role-based access and privacy-aware sharing
- Alerts, budgets, anomaly detection, and usage recommendations
- Plugin/importer system for custom harnesses

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
