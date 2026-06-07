import React, { useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Activity,
  AlertTriangle,
  BadgeDollarSign,
  Boxes,
  Database,
  FileSearch,
  Gauge,
  HardDrive,
  ListChecks,
  Lock,
  Network,
  Search,
  Settings,
  ShieldCheck,
  Sparkles,
  Terminal,
  Zap
} from "lucide-react";
import "./styles.css";

type UsageTotals = {
  prompt_tokens: number;
  completion_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number;
  total_tokens: number;
  estimated_cost_usd: number;
};

type NamedUsagePoint = {
  name: string;
  prompt_tokens: number;
  completion_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  total_tokens: number;
  estimated_cost_usd: number;
};

type SessionSummary = {
  machine: string;
  source: string;
  session_id: string;
  project_path: string;
  provider: string;
  model: string;
  total_tokens: number;
  estimated_cost_usd: number;
  confidence: number;
  first_seen?: string;
  last_seen?: string;
  raw_path: string;
  parser_name: string;
  pricing_version: string;
};

type DashboardSummary = {
  totals: UsageTotals;
  cache: {
    cache_read_tokens: number;
    cache_write_tokens: number;
    cache_read_share: number;
    hit_ratio: number;
    estimated_savings_usd: number;
  };
  daily: NamedUsagePoint[];
  by_source: NamedUsagePoint[];
  by_model: NamedUsagePoint[];
  by_project: NamedUsagePoint[];
  expensive_sessions: SessionSummary[];
};

type SourceSummary = {
  source: string;
  machine: string;
  files: number;
  parse_errors: number;
  last_imported_at?: string;
};

type PricingRecord = {
  provider: string;
  model: string;
  input_rate: number;
  output_rate: number;
  cache_read_rate: number;
  cache_write_rate: number;
  source_label: string;
  snapshot_version: string;
  override_flag: boolean;
  local_free_flag: boolean;
};

type DoctorReport = {
  event_count: number;
  pricing_count: number;
  detected_sources: number;
  warnings: string[];
};

const emptySummary: DashboardSummary = {
  totals: {
    prompt_tokens: 0,
    completion_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    reasoning_tokens: 0,
    total_tokens: 0,
    estimated_cost_usd: 0
  },
  cache: {
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    cache_read_share: 0,
    hit_ratio: 0,
    estimated_savings_usd: 0
  },
  daily: [],
  by_source: [],
  by_model: [],
  by_project: [],
  expensive_sessions: []
};

const navItems = [
  ["Overview", Gauge],
  ["The Sink", Activity],
  ["Sources", HardDrive],
  ["Sessions", Terminal],
  ["Projects", Boxes],
  ["Models", Sparkles],
  ["Cache", Zap],
  ["Burn Report", AlertTriangle],
  ["Import/Files", FileSearch],
  ["Pricing", BadgeDollarSign],
  ["Privacy", Lock],
  ["Settings", Settings],
  ["Doctor", ListChecks]
] as const;

function App() {
  const [page, setPage] = useState("Overview");
  const [summary, setSummary] = useState<DashboardSummary>(emptySummary);
  const [sources, setSources] = useState<SourceSummary[]>([]);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [pricing, setPricing] = useState<PricingRecord[]>([]);
  const [doctor, setDoctor] = useState<DoctorReport | null>(null);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    async function load() {
      setLoading(true);
      setError(null);
      try {
        const [summaryData, sourcesData, sessionsData, pricingData, doctorData] =
          await Promise.all([
            fetchJson<DashboardSummary>("/api/summary"),
            fetchJson<SourceSummary[]>("/api/sources"),
            fetchJson<SessionSummary[]>("/api/sessions"),
            fetchJson<PricingRecord[]>("/api/pricing"),
            fetchJson<DoctorReport>("/api/doctor")
          ]);
        if (!active) return;
        setSummary(summaryData);
        setSources(sourcesData);
        setSessions(sessionsData);
        setPricing(pricingData);
        setDoctor(doctorData);
      } catch (loadError) {
        if (active) setError(loadError instanceof Error ? loadError.message : "Failed to load");
      } finally {
        if (active) setLoading(false);
      }
    }
    load();
    return () => {
      active = false;
    };
  }, []);

  const filteredSessions = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    if (!normalized) return sessions;
    return sessions.filter((session) =>
      [
        session.session_id,
        session.project_path,
        session.source,
        session.machine,
        session.model,
        session.raw_path
      ]
        .join(" ")
        .toLowerCase()
        .includes(normalized)
    );
  }, [query, sessions]);

  return (
    <main className="app-shell">
      <aside className="sidebar" aria-label="Primary">
        <div className="brand-lockup">
          <Terminal size={20} aria-hidden="true" />
          <div>
            <strong>dirtydash</strong>
            <span>terminal observatory</span>
          </div>
        </div>
        <nav className="nav-list">
          {navItems.map(([label, Icon]) => (
            <button
              key={label}
              type="button"
              className={page === label ? "nav-item active" : "nav-item"}
              onClick={() => setPage(label)}
              title={label}
            >
              <Icon size={16} aria-hidden="true" />
              <span>{label}</span>
            </button>
          ))}
        </nav>
        <div className="trust-strip">
          <ShieldCheck size={16} aria-hidden="true" />
          <span>metadata-only by default</span>
        </div>
      </aside>

      <section className="workspace">
        <header className="topbar">
          <div>
            <p className="kicker">local SQLite, bundled pricing, provenance nearby</p>
            <h1>{page}</h1>
          </div>
          <label className="search-box">
            <Search size={16} aria-hidden="true" />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search sessions, projects, models"
            />
          </label>
        </header>

        {loading ? <Skeleton /> : null}
        {error ? <Notice tone="danger" text={error} /> : null}
        {!loading && !error ? (
          <Page
            page={page}
            summary={summary}
            sources={sources}
            sessions={filteredSessions}
            pricing={pricing}
            doctor={doctor}
          />
        ) : null}
      </section>
    </main>
  );
}

async function fetchJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} returned ${response.status}`);
  return (await response.json()) as T;
}

function Page({
  page,
  summary,
  sources,
  sessions,
  pricing,
  doctor
}: {
  page: string;
  summary: DashboardSummary;
  sources: SourceSummary[];
  sessions: SessionSummary[];
  pricing: PricingRecord[];
  doctor: DoctorReport | null;
}) {
  if (page === "Sources") return <SourcesPage sources={sources} />;
  if (page === "Sessions") return <SessionsPage sessions={sessions} />;
  if (page === "Projects") return <Breakdown title="Project totals" rows={summary.by_project} />;
  if (page === "Models") return <Breakdown title="Model totals" rows={summary.by_model} />;
  if (page === "Cache") return <CachePage summary={summary} />;
  if (page === "Burn Report") return <BurnReport summary={summary} />;
  if (page === "Import/Files") return <SourcesPage sources={sources} filesMode />;
  if (page === "Pricing") return <PricingPage pricing={pricing} />;
  if (page === "Doctor") return <DoctorPage doctor={doctor} />;
  if (page === "Privacy") return <PrivacyPage />;
  if (page === "Settings") return <SettingsPage />;
  if (page === "The Sink") return <SinkPage summary={summary} sessions={sessions} />;
  return <Overview summary={summary} sessions={sessions} sources={sources} />;
}

function Overview({
  summary,
  sessions,
  sources
}: {
  summary: DashboardSummary;
  sessions: SessionSummary[];
  sources: SourceSummary[];
}) {
  return (
    <div className="page-grid">
      <Metric label="Estimated spend" value={money(summary.totals.estimated_cost_usd)} sub="reported, bundled, manual pricing" />
      <Metric label="Total tokens" value={compact(summary.totals.total_tokens)} sub="prompt, output, cache, reasoning" />
      <Metric label="Cache read share" value={percent(summary.cache.cache_read_share)} sub={`${compact(summary.cache.cache_read_tokens)} observed reads`} />
      <Metric label="Sources" value={sources.length.toString()} sub="local plus SSH-pulled metadata" />
      <TrendPanel title="Usage by source" rows={summary.by_source} />
      <TrendPanel title="Usage by model" rows={summary.by_model} />
      <SessionsTable title="Top expensive sessions" sessions={sessions.slice(0, 8)} />
    </div>
  );
}

function SinkPage({
  summary,
  sessions
}: {
  summary: DashboardSummary;
  sessions: SessionSummary[];
}) {
  return (
    <div className="page-grid">
      <Metric label="Machines" value={new Set(sessions.map((session) => session.machine)).size.toString()} sub="combined local and remote provenance" />
      <Metric label="Sink tokens" value={compact(summary.totals.total_tokens)} sub="all imported usage events" />
      <Breakdown title="Machine and source totals" rows={summary.by_source} />
      <SessionsTable title="Recent sink sessions" sessions={sessions.slice(0, 12)} />
    </div>
  );
}

function CachePage({ summary }: { summary: DashboardSummary }) {
  const observedInputTokens =
    summary.totals.prompt_tokens +
    summary.totals.cache_read_tokens +
    summary.totals.cache_write_tokens;
  const cacheWriteSub =
    summary.totals.cache_write_tokens > 0
      ? "source-reported cache creation"
      : "not exposed by current logs";
  const rows: NamedUsagePoint[] = [
    {
      name: "uncached input",
      prompt_tokens: summary.totals.prompt_tokens,
      completion_tokens: 0,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      total_tokens: summary.totals.prompt_tokens,
      estimated_cost_usd: 0
    },
    {
      name: "cache read",
      prompt_tokens: 0,
      completion_tokens: 0,
      cache_read_tokens: summary.totals.cache_read_tokens,
      cache_write_tokens: 0,
      total_tokens: summary.totals.cache_read_tokens,
      estimated_cost_usd: 0
    },
    {
      name: "cache write",
      prompt_tokens: 0,
      completion_tokens: 0,
      cache_read_tokens: 0,
      cache_write_tokens: summary.totals.cache_write_tokens,
      total_tokens: summary.totals.cache_write_tokens,
      estimated_cost_usd: 0
    }
  ];
  return (
    <div className="page-grid">
      <Metric label="Cache read share" value={percent(summary.cache.cache_read_share)} sub={`${compact(summary.totals.cache_read_tokens)} of ${compact(observedInputTokens)} input`} />
      <Metric label="Cache reads" value={compact(summary.totals.cache_read_tokens)} sub="cached input reported by logs" />
      <Metric label="Reported writes" value={compact(summary.totals.cache_write_tokens)} sub={cacheWriteSub} />
      <section className="panel note-panel">
        <div className="panel-header">
          <h2>Accounting note</h2>
          <span>observed only</span>
        </div>
        <p>
          Dirtydash only counts cache lifecycle tokens present in local session logs. Codex logs
          usually expose cached input reads, but not cache creation/write tokens.
        </p>
      </section>
      <Breakdown title="Cache behavior" rows={rows} />
    </div>
  );
}

function BurnReport({ summary }: { summary: DashboardSummary }) {
  const unpriced = summary.by_model.filter((row) => row.total_tokens > 0 && row.estimated_cost_usd === 0);
  const unpricedTokens = unpriced.reduce((total, row) => total + row.total_tokens, 0);
  return (
    <div className="page-grid">
      <Metric label="Biggest session" value={summary.expensive_sessions[0] ? money(summary.expensive_sessions[0].estimated_cost_usd) : "$0.00"} sub={summary.expensive_sessions[0]?.session_id ?? "no sessions yet"} />
      <Metric label="Biggest model" value={summary.by_model[0]?.name ?? "unknown"} sub={summary.by_model[0] ? money(summary.by_model[0].estimated_cost_usd) : "no spend"} />
      <Metric label="Unpriced tokens" value={compact(unpricedTokens)} sub={`${unpriced.length} model rows need pricing`} />
      <SessionsTable title="Sessions to inspect first" sessions={summary.expensive_sessions} />
    </div>
  );
}

function SourcesPage({ sources, filesMode = false }: { sources: SourceSummary[]; filesMode?: boolean }) {
  return (
    <section className="panel wide">
      <div className="panel-header">
        <h2>{filesMode ? "Tracked import files" : "Detected sources"}</h2>
        <span>{sources.length} source rows</span>
      </div>
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Source</th>
              <th>Machine</th>
              <th>Files</th>
              <th>Parse errors</th>
              <th>Last import</th>
            </tr>
          </thead>
          <tbody>
            {sources.map((source) => (
              <tr key={`${source.source}-${source.machine}`}>
                <td>{source.source}</td>
                <td>{source.machine}</td>
                <td>{source.files}</td>
                <td>
                  <Status value={source.parse_errors === 0 ? "clean" : `${source.parse_errors} errors`} tone={source.parse_errors === 0 ? "good" : "warn"} />
                </td>
                <td>{source.last_imported_at ? shortDate(source.last_imported_at) : "-"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function SessionsPage({ sessions }: { sessions: SessionSummary[] }) {
  return <SessionsTable title="Searchable sessions" sessions={sessions} />;
}

function PricingPage({ pricing }: { pricing: PricingRecord[] }) {
  return (
    <section className="panel wide">
      <div className="panel-header">
        <h2>Pricing snapshot</h2>
        <span>{pricing.length} model records</span>
      </div>
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Provider</th>
              <th>Model</th>
              <th>Input</th>
              <th>Output</th>
              <th>Cache read</th>
              <th>Cache write</th>
              <th>Source</th>
            </tr>
          </thead>
          <tbody>
            {pricing.map((row) => (
              <tr key={`${row.provider}-${row.model}`}>
                <td>{row.provider}</td>
                <td>{row.model}</td>
                <td>{money(row.input_rate)}</td>
                <td>{money(row.output_rate)}</td>
                <td>{money(row.cache_read_rate)}</td>
                <td>{money(row.cache_write_rate)}</td>
                <td>
                  <Status
                    value={row.local_free_flag ? "free" : row.override_flag ? "override" : row.snapshot_version}
                    tone={row.override_flag || row.local_free_flag ? "info" : "neutral"}
                  />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function DoctorPage({ doctor }: { doctor: DoctorReport | null }) {
  if (!doctor) return <Notice tone="warn" text="Doctor data is not available yet." />;
  return (
    <div className="page-grid">
      <Metric label="Events" value={doctor.event_count.toString()} sub="usage rows in SQLite" />
      <Metric label="Pricing records" value={doctor.pricing_count.toString()} sub="bundled plus overrides" />
      <Metric label="Detected sources" value={doctor.detected_sources.toString()} sub="paths with files" />
      <section className="panel wide">
        <div className="panel-header">
          <h2>Warnings</h2>
          <span>{doctor.warnings.length}</span>
        </div>
        {doctor.warnings.length === 0 ? (
          <Notice tone="good" text="No doctor warnings were reported." />
        ) : (
          doctor.warnings.map((warning) => <Notice key={warning} tone="warn" text={warning} />)
        )}
      </section>
    </div>
  );
}

function PrivacyPage() {
  return (
    <section className="panel wide">
      <div className="panel-header">
        <h2>Privacy posture</h2>
        <span>metadata-first</span>
      </div>
      <div className="detail-grid">
        <Detail label="Default import" value="metadata-only" />
        <Detail label="Stored provenance" value="raw path, raw span, parser, event hash" />
        <Detail label="Preview handling" value="not requested during first-run happy path" />
        <Detail label="Remote behavior" value="pull discovery over SSH, no remote agent install" />
      </div>
    </section>
  );
}

function SettingsPage() {
  return (
    <section className="panel wide">
      <div className="panel-header">
        <h2>Settings surface</h2>
        <span>CLI-backed</span>
      </div>
      <div className="command-list">
        <code>dirtydash scan</code>
        <code>dirtydash import --metadata-only</code>
        <code>dirtydash pricing list</code>
        <code>dirtydash remote list</code>
        <code>dirtydash doctor</code>
      </div>
    </section>
  );
}

function Breakdown({ title, rows }: { title: string; rows: NamedUsagePoint[] }) {
  return <TrendPanel title={title} rows={rows} className="wide" />;
}

function TrendPanel({ title, rows, className = "" }: { title: string; rows: NamedUsagePoint[]; className?: string }) {
  const max = Math.max(1, ...rows.map((row) => row.total_tokens));
  return (
    <section className={`panel ${className}`}>
      <div className="panel-header">
        <h2>{title}</h2>
        <span>{rows.length} rows</span>
      </div>
      <div className="bar-list">
        {rows.length === 0 ? <Empty text="No imported usage yet." /> : null}
        {rows.map((row) => (
          <div className="bar-row" key={row.name}>
            <div className="bar-label">
              <span>{row.name || "unknown"}</span>
              <small>{money(row.estimated_cost_usd)}</small>
            </div>
            <div className="bar-track" aria-hidden="true">
              <span style={{ width: `${Math.max(4, (row.total_tokens / max) * 100)}%` }} />
            </div>
            <small>{compact(row.total_tokens)} tokens</small>
          </div>
        ))}
      </div>
    </section>
  );
}

function SessionsTable({ title, sessions }: { title: string; sessions: SessionSummary[] }) {
  return (
    <section className="panel wide">
      <div className="panel-header">
        <h2>{title}</h2>
        <span>{sessions.length} sessions</span>
      </div>
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Session</th>
              <th>Source</th>
              <th>Project</th>
              <th>Model</th>
              <th>Tokens</th>
              <th>Cost</th>
              <th>Provenance</th>
            </tr>
          </thead>
          <tbody>
            {sessions.map((session) => (
              <tr key={`${session.machine}-${session.source}-${session.session_id}-${session.model}`}>
                <td>{session.session_id}</td>
                <td>{session.source}</td>
                <td>{session.project_path}</td>
                <td>{session.model}</td>
                <td>{compact(session.total_tokens)}</td>
                <td>{money(session.estimated_cost_usd)}</td>
                <td>
                  <span className="provenance">{session.parser_name}</span>
                  <span className="pricing-version">{session.pricing_version}</span>
                  <span className="raw-path">{session.raw_path}</span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {sessions.length === 0 ? <Empty text="No sessions match the current filter." /> : null}
      </div>
    </section>
  );
}

function Metric({ label, value, sub }: { label: string; value: string; sub: string }) {
  return (
    <section className="metric">
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{sub}</small>
    </section>
  );
}

function Detail({ label, value }: { label: string; value: string }) {
  return (
    <div className="detail">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function Status({ value, tone }: { value: string; tone: "good" | "warn" | "danger" | "info" | "neutral" }) {
  return <span className={`status ${tone}`}>{value}</span>;
}

function Notice({ text, tone }: { text: string; tone: "good" | "warn" | "danger" }) {
  return <div className={`notice ${tone}`}>{text}</div>;
}

function Skeleton() {
  return (
    <div className="page-grid">
      <div className="skeleton" />
      <div className="skeleton" />
      <div className="skeleton" />
      <div className="skeleton wide" />
    </div>
  );
}

function Empty({ text }: { text: string }) {
  return <p className="empty">{text}</p>;
}

function compact(value: number) {
  return Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(value);
}

function money(value: number) {
  return Intl.NumberFormat(undefined, {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: value < 10 ? 4 : 2
  }).format(value);
}

function percent(value: number) {
  return `${Math.round(value * 100)}%`;
}

function shortDate(value: string) {
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit"
  }).format(new Date(value));
}

createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
