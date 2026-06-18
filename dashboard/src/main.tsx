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
  standard_tokens: number;
  priority_tokens: number;
  priority_estimated_cost_usd: number;
};

type NamedUsagePoint = {
  name: string;
  prompt_tokens: number;
  completion_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number;
  total_tokens: number;
  estimated_cost_usd: number;
  standard_tokens: number;
  priority_tokens: number;
  priority_estimated_cost_usd: number;
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

type UsageBarMetric = "tokens" | "cost";

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
    estimated_cost_usd: 0,
    standard_tokens: 0,
    priority_tokens: 0,
    priority_estimated_cost_usd: 0
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

type MockRoute = "mock1" | "mock2" | "mock3" | "mock4";

const mockNavItems: { route: MockRoute; href: string; label: string; note: string }[] = [
  { route: "mock1", href: "/mock1", label: "Run Ledger", note: "cost + cache spine" },
  { route: "mock2", href: "/mock2", label: "Source Matrix", note: "machines x models" },
  { route: "mock3", href: "/mock3", label: "Session Inspector", note: "search + provenance" },
  { route: "mock4", href: "/mock4", label: "Local Ops", note: "import, pricing, doctor" }
];

const mockSessions = [
  {
    id: "sess_07f4a9",
    project: "~/dev/dirtydash",
    source: "codex",
    model: "gpt-5-codex",
    tokens: 1849200,
    cost: 42.18,
    cache: 0.63,
    status: "priced"
  },
  {
    id: "sess_91dd20",
    project: "~/work/ledgerctl",
    source: "claude-code",
    model: "opus-4.1",
    tokens: 1102400,
    cost: 31.92,
    cache: 0.41,
    status: "needs review"
  },
  {
    id: "sess_c8b73e",
    project: "~/dev/dirtydash",
    source: "codex",
    model: "gpt-5-mini",
    tokens: 722800,
    cost: 5.77,
    cache: 0.78,
    status: "priced"
  },
  {
    id: "sess_448af1",
    project: "~/lab/parser-fixtures",
    source: "cursor",
    model: "gpt-4.1",
    tokens: 391400,
    cost: 7.64,
    cache: 0.18,
    status: "partial"
  }
];

const mockSources = [
  { name: "local-mbp", tool: "codex", files: 348, errors: 0, freshness: "41s", tokens: 2920000 },
  { name: "rack-mini", tool: "claude-code", files: 112, errors: 2, freshness: "9m", tokens: 1040000 },
  { name: "studio-linux", tool: "cursor", files: 64, errors: 1, freshness: "22m", tokens: 580000 },
  { name: "archive", tool: "codexbar", files: 19, errors: 0, freshness: "2h", tokens: 210000 }
];

const mockModels = [
  { name: "gpt-5-codex", tokens: 1849200, cost: 42.18, cache: 0.63 },
  { name: "opus-4.1", tokens: 1102400, cost: 31.92, cache: 0.41 },
  { name: "gpt-5-mini", tokens: 722800, cost: 5.77, cache: 0.78 },
  { name: "gpt-4.1", tokens: 391400, cost: 7.64, cache: 0.18 }
];

function App() {
  const mockRoute = getMockRoute(window.location.pathname);
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
    if (mockRoute) {
      setLoading(false);
      setError(null);
      return;
    }
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
  }, [mockRoute]);

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

  if (mockRoute) return <MockRoutePage route={mockRoute} />;

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

function getMockRoute(pathname: string): MockRoute | null {
  const route = pathname.replace(/^\/+/, "").split("/")[0];
  return route === "mock1" || route === "mock2" || route === "mock3" || route === "mock4"
    ? route
    : null;
}

async function fetchJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} returned ${response.status}`);
  return (await response.json()) as T;
}

type MockSort = "cost" | "tokens" | "cache";
type MockView = "inspect" | "trace" | "compare";

const mockSorts: MockSort[] = ["cost", "tokens", "cache"];
const mockViews: MockView[] = ["inspect", "trace", "compare"];

function MockRoutePage({ route }: { route: MockRoute }) {
  const [sort, setSort] = useState<MockSort>("cost");
  const [view, setView] = useState<MockView>("inspect");
  const [activeIndex, setActiveIndex] = useState(0);
  const routeIndex = mockNavItems.findIndex((item) => item.route === route);
  const sortedSessions = useMemo(() => {
    const key =
      sort === "cost"
        ? (session: (typeof mockSessions)[number]) => session.cost
        : sort === "tokens"
          ? (session: (typeof mockSessions)[number]) => session.tokens
          : (session: (typeof mockSessions)[number]) => session.cache;
    return [...mockSessions].sort((a, b) => key(b) - key(a));
  }, [sort]);
  const activeSession = sortedSessions[Math.min(activeIndex, sortedSessions.length - 1)];

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (event.key >= "1" && event.key <= "4") {
        const target = mockNavItems[Number(event.key) - 1];
        if (target) window.location.href = target.href;
      }
      if (event.key.toLowerCase() === "s") {
        event.preventDefault();
        setSort((current) => cycleValue(mockSorts, current));
      }
      if (event.key.toLowerCase() === "v") {
        event.preventDefault();
        setView((current) => cycleValue(mockViews, current));
      }
      if (event.key.toLowerCase() === "j") {
        event.preventDefault();
        setActiveIndex((current) => Math.min(sortedSessions.length - 1, current + 1));
      }
      if (event.key.toLowerCase() === "k") {
        event.preventDefault();
        setActiveIndex((current) => Math.max(0, current - 1));
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [sortedSessions.length]);

  useEffect(() => {
    setActiveIndex((current) => Math.min(current, sortedSessions.length - 1));
  }, [sortedSessions.length]);

  return (
    <main className={`mock-shell mock-${route}`}>
      <aside className="mock-rail" aria-label="Mockup routes">
        <a className="mock-brand" href="/">
          <Terminal size={18} aria-hidden="true" />
          <span>dirtydash</span>
          <code>mock/{routeIndex + 1}</code>
        </a>
        <nav className="mock-route-list">
          {mockNavItems.map((item, index) => (
            <a
              key={item.route}
              className={item.route === route ? "mock-route active" : "mock-route"}
              href={item.href}
            >
              <kbd>{index + 1}</kbd>
              <span>{item.label}</span>
              <small>{item.note}</small>
            </a>
          ))}
        </nav>
        <div className="mock-hotkeys">
          <span>hotkeys</span>
          <dl>
            <div>
              <dt>s</dt>
              <dd>sort {sort}</dd>
            </div>
            <div>
              <dt>v</dt>
              <dd>view {view}</dd>
            </div>
            <div>
              <dt>j/k</dt>
              <dd>move cursor</dd>
            </div>
          </dl>
        </div>
      </aside>

      <section className="mock-workspace">
        <MockCommandBar route={route} sort={sort} view={view} setSort={setSort} setView={setView} />
        {route === "mock1" ? (
          <MockRunLedger sessions={sortedSessions} activeSession={activeSession} activeIndex={activeIndex} sort={sort} view={view} />
        ) : null}
        {route === "mock2" ? (
          <MockSourceMatrix sessions={sortedSessions} activeSession={activeSession} activeIndex={activeIndex} sort={sort} view={view} />
        ) : null}
        {route === "mock3" ? (
          <MockSessionInspector sessions={sortedSessions} activeSession={activeSession} activeIndex={activeIndex} sort={sort} view={view} />
        ) : null}
        {route === "mock4" ? (
          <MockLocalOps sessions={sortedSessions} activeSession={activeSession} sort={sort} view={view} />
        ) : null}
      </section>
    </main>
  );
}

function MockCommandBar({
  route,
  sort,
  view,
  setSort,
  setView
}: {
  route: MockRoute;
  sort: MockSort;
  view: MockView;
  setSort: React.Dispatch<React.SetStateAction<MockSort>>;
  setView: React.Dispatch<React.SetStateAction<MockView>>;
}) {
  const title = mockNavItems.find((item) => item.route === route)?.label ?? "Mockup";
  return (
    <header className="mock-command">
      <div className="mock-prompt" aria-label="Current command context">
        <span>dirtydash</span>
        <code>{title.toLowerCase().replace(/\s+/g, "-")}</code>
        <span>--sort</span>
        <code>{sort}</code>
        <span>--view</span>
        <code>{view}</code>
      </div>
      <div className="mock-controls" aria-label="Mockup controls">
        {mockSorts.map((value) => (
          <button
            key={value}
            type="button"
            className={value === sort ? "active" : ""}
            onClick={() => setSort(value)}
          >
            sort:{value}
          </button>
        ))}
        {mockViews.map((value) => (
          <button
            key={value}
            type="button"
            className={value === view ? "active" : ""}
            onClick={() => setView(value)}
          >
            view:{value}
          </button>
        ))}
      </div>
    </header>
  );
}

function MockRunLedger({
  sessions,
  activeSession,
  activeIndex,
  sort,
  view
}: {
  sessions: typeof mockSessions;
  activeSession: (typeof mockSessions)[number];
  activeIndex: number;
  sort: MockSort;
  view: MockView;
}) {
  return (
    <div className="mock-layout ledger-layout">
      <section className="mock-primary-pane">
        <MockHeadline
          title="Run ledger"
          text="A cost-and-cache spine for developers who want the interesting rows first, with keyboard sorting and a cursor that never hides provenance."
        />
        <div className="mock-stat-strip">
          <MockStat label="observed spend" value="$87.51" detail="4 priced tools" />
          <MockStat label="cache read" value="61%" detail="input incl. reads" />
          <MockStat label="unpriced" value="2 rows" detail="parser needs price" />
          <MockStat label="freshness" value="41s" detail="local-mbp import" />
        </div>
        <div className="mock-ledger" role="table" aria-label="Run ledger sorted sessions">
          <div className="mock-ledger-head" role="row">
            <span>cursor</span>
            <span>session</span>
            <span>model</span>
            <span>tokens</span>
            <span>cost</span>
            <span>cache</span>
            <span>status</span>
          </div>
          {sessions.map((session, index) => (
            <div
              key={session.id}
              className={index === activeIndex ? "mock-ledger-row active" : "mock-ledger-row"}
              role="row"
            >
              <span aria-label={index === activeIndex ? "selected" : "not selected"}>
                {index === activeIndex ? ">" : " "}
              </span>
              <code>{session.id}</code>
              <span>{session.model}</span>
              <span>{compact(session.tokens)}</span>
              <span>{money(session.cost)}</span>
              <MockInlineMeter value={session.cache} label={`${percent(session.cache)} cache read`} />
              <span>{session.status}</span>
            </div>
          ))}
        </div>
      </section>
      <MockInspector activeSession={activeSession} sort={sort} view={view} />
    </div>
  );
}

function MockSourceMatrix({
  sessions,
  activeSession,
  activeIndex,
  sort,
  view
}: {
  sessions: typeof mockSessions;
  activeSession: (typeof mockSessions)[number];
  activeIndex: number;
  sort: MockSort;
  view: MockView;
}) {
  return (
    <div className="mock-layout matrix-layout">
      <section className="mock-primary-pane">
        <MockHeadline
          title="Source matrix"
          text="Machines, tools, and models share one dense surface: the user can see which importer is stale, which model is expensive, and where cache behavior is trustworthy."
        />
        <div className="mock-matrix" role="table" aria-label="Source and model usage matrix">
          <div className="mock-matrix-head" role="row">
            <span>source</span>
            {mockModels.map((model) => (
              <span key={model.name}>{model.name}</span>
            ))}
            <span>fresh</span>
          </div>
          {mockSources.map((source, sourceIndex) => (
            <div key={source.name} className="mock-matrix-row" role="row">
              <span>
                <code>{source.name}</code>
                <small>{source.tool} / {source.files} files</small>
              </span>
              {mockModels.map((model, modelIndex) => {
                const weight = ((sourceIndex + 2) * (modelIndex + 3)) % 10;
                return (
                  <span key={model.name} className="mock-cell" data-weight={weight}>
                    {weight > 2 ? compact(Math.round((source.tokens * (weight + 2)) / 18)) : "-"}
                  </span>
                );
              })}
              <span className={source.errors > 0 ? "mock-warn-text" : "mock-good-text"}>
                {source.errors > 0 ? `${source.errors} err` : source.freshness}
              </span>
            </div>
          ))}
        </div>
        <div className="mock-log-pane" aria-label="Import log excerpt">
          <p><code>import</code> local-mbp/codex parsed 348 files, 0 errors, 41s ago</p>
          <p><code>price</code> gpt-5-codex mapped to bundled snapshot 2026-06</p>
          <p><code>warn</code> rack-mini/claude-code has 2 records without final stop timestamp</p>
        </div>
      </section>
      <MockInspector activeSession={activeSession ?? sessions[activeIndex]} sort={sort} view={view} />
    </div>
  );
}

function MockSessionInspector({
  sessions,
  activeSession,
  activeIndex,
  sort,
  view
}: {
  sessions: typeof mockSessions;
  activeSession: (typeof mockSessions)[number];
  activeIndex: number;
  sort: MockSort;
  view: MockView;
}) {
  return (
    <div className="mock-layout inspector-layout">
      <section className="mock-primary-pane">
        <MockHeadline
          title="Session inspector"
          text="Search behaves like a command buffer, but the result surface stays web-native: sticky columns, readable provenance, and fast sort pivots."
        />
        <div className="mock-command-line" aria-label="Mock search command">
          <Search size={15} aria-hidden="true" />
          <span>session where project:dirtydash sort:{sort} view:{view}</span>
          <kbd>/</kbd>
        </div>
        <div className="mock-session-list">
          {sessions.map((session, index) => (
            <div key={session.id} className={index === activeIndex ? "mock-session-row active" : "mock-session-row"}>
              <span>{index === activeIndex ? ">" : " "}</span>
              <code>{session.id}</code>
              <span>{session.project}</span>
              <span>{session.source}</span>
              <span>{compact(session.tokens)}</span>
              <span>{money(session.cost)}</span>
            </div>
          ))}
        </div>
        <div className="mock-trace">
          <span>trace</span>
          <ol>
            <li>raw span located in <code>~/.codex/sessions/2026/06/18/{activeSession.id}.jsonl</code></li>
            <li>parser normalized prompt, cache read, output, and reasoning tokens</li>
            <li>pricing snapshot attached before rollup so cost remains auditable</li>
          </ol>
        </div>
      </section>
      <MockInspector activeSession={activeSession} sort={sort} view={view} />
    </div>
  );
}

function MockLocalOps({
  sessions,
  activeSession,
  sort,
  view
}: {
  sessions: typeof mockSessions;
  activeSession: (typeof mockSessions)[number];
  sort: MockSort;
  view: MockView;
}) {
  return (
    <div className="mock-layout ops-layout">
      <section className="mock-primary-pane">
        <MockHeadline
          title="Local ops"
          text="A durable control surface for imports, doctor checks, and pricing overrides. It keeps commands visible but gives users web-grade scanning and safer state changes."
        />
        <div className="mock-ops-grid">
          <MockOpsLane title="import queue" rows={["codex local ready", "rack-mini ssh stale", "cursor archive paused"]} />
          <MockOpsLane title="doctor" rows={["0 schema drift", "2 parse warnings", "pricing snapshot current"]} />
          <MockOpsLane title="privacy" rows={["metadata-only default", "no remote agent install", "raw previews disabled"]} />
        </div>
        <div className="mock-runbook">
          <span>next commands</span>
          <code>dirtydash import --metadata-only --source codex</code>
          <code>dirtydash doctor --explain</code>
          <code>dirtydash pricing list --overrides</code>
        </div>
      </section>
      <MockInspector activeSession={activeSession ?? sessions[0]} sort={sort} view={view} />
    </div>
  );
}

function MockHeadline({ title, text }: { title: string; text: string }) {
  return (
    <div className="mock-headline">
      <div>
        <h1>{title}</h1>
        <p>{text}</p>
      </div>
      <div className="mock-status-line">
        <Status value="local" tone="good" />
        <Status value="no cards" tone="info" />
        <Status value="keyboard-first" tone="neutral" />
      </div>
    </div>
  );
}

function MockStat({ label, value, detail }: { label: string; value: string; detail: string }) {
  return (
    <div className="mock-stat">
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{detail}</small>
    </div>
  );
}

function MockInspector({
  activeSession,
  sort,
  view
}: {
  activeSession: (typeof mockSessions)[number];
  sort: MockSort;
  view: MockView;
}) {
  return (
    <aside className="mock-inspector" aria-label="Selected session inspector">
      <div className="mock-inspector-title">
        <span>selected</span>
        <code>{activeSession.id}</code>
      </div>
      <dl className="mock-detail-list">
        <div>
          <dt>project</dt>
          <dd>{activeSession.project}</dd>
        </div>
        <div>
          <dt>model</dt>
          <dd>{activeSession.model}</dd>
        </div>
        <div>
          <dt>tokens</dt>
          <dd>{compact(activeSession.tokens)}</dd>
        </div>
        <div>
          <dt>cost</dt>
          <dd>{money(activeSession.cost)}</dd>
        </div>
        <div>
          <dt>cache read</dt>
          <dd>{percent(activeSession.cache)}</dd>
        </div>
        <div>
          <dt>mode</dt>
          <dd>{view} / sort:{sort}</dd>
        </div>
      </dl>
      <div className="mock-provenance">
        <span>provenance</span>
        <code>parser=codex-jsonl</code>
        <code>pricing=bundled-2026-06</code>
        <code>raw=~/.codex/sessions/.../{activeSession.id}.jsonl</code>
      </div>
    </aside>
  );
}

function MockInlineMeter({ value, label }: { value: number; label: string }) {
  return (
    <span className="mock-inline-meter" role="img" aria-label={label} title={label}>
      <span style={{ width: `${Math.round(value * 100)}%` }} />
    </span>
  );
}

function MockOpsLane({ title, rows }: { title: string; rows: string[] }) {
  return (
    <div className="mock-ops-lane">
      <h2>{title}</h2>
      {rows.map((row) => (
        <p key={row}>
          <span aria-hidden="true">::</span>
          {row}
        </p>
      ))}
    </div>
  );
}

function cycleValue<T>(values: T[], current: T): T {
  const index = values.indexOf(current);
  return values[(index + 1) % values.length];
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
  if (page === "Pricing") return <PricingPage pricing={pricing} usageRows={summary.by_model} />;
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
  const observedInput =
    summary.totals.prompt_tokens +
    summary.totals.cache_read_tokens +
    summary.totals.cache_write_tokens;
  const generatedTokens = summary.totals.completion_tokens + summary.totals.reasoning_tokens;
  return (
    <div className="page-grid">
      <Metric label="Estimated spend" value={money(summary.totals.estimated_cost_usd)} sub="reported, manual, local CodexBar pricing" />
      <Metric label="Total tokens" value={compact(summary.totals.total_tokens)} sub={`${compact(observedInput)} input incl. cache, ${compact(generatedTokens)} generated`} />
      <Metric label="Cache read share" value={percent(summary.cache.cache_read_share)} sub={`${compact(summary.cache.cache_read_tokens)} observed reads`} />
      <Metric label="Sources" value={sources.length.toString()} sub="local plus SSH-pulled metadata" />
      <TrendPanel title="Token usage over time" rows={summary.daily} />
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
      reasoning_tokens: 0,
      total_tokens: summary.totals.prompt_tokens,
      estimated_cost_usd: 0,
      standard_tokens: summary.totals.prompt_tokens,
      priority_tokens: 0,
      priority_estimated_cost_usd: 0
    },
    {
      name: "cache read",
      prompt_tokens: 0,
      completion_tokens: 0,
      cache_read_tokens: summary.totals.cache_read_tokens,
      cache_write_tokens: 0,
      reasoning_tokens: 0,
      total_tokens: summary.totals.cache_read_tokens,
      estimated_cost_usd: 0,
      standard_tokens: summary.totals.cache_read_tokens,
      priority_tokens: 0,
      priority_estimated_cost_usd: 0
    },
    {
      name: "cache write",
      prompt_tokens: 0,
      completion_tokens: 0,
      cache_read_tokens: 0,
      cache_write_tokens: summary.totals.cache_write_tokens,
      reasoning_tokens: 0,
      total_tokens: summary.totals.cache_write_tokens,
      estimated_cost_usd: 0,
      standard_tokens: summary.totals.cache_write_tokens,
      priority_tokens: 0,
      priority_estimated_cost_usd: 0
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
      <Metric label="Codex limit drain" value="not measured" sub="fast mode is not inferred from cache reads" />
      <TrendPanel title="Model spend" rows={summary.by_model} metric="cost" />
      <AccountingNote
        title="Codex subscription accounting"
        badge="separate ledger"
      >
        Dirtydash estimates tokenized API-style cost from imported logs. Codex subscription
        limits can drain from a different fast-mode ledger, especially when xhigh sessions
        produce uncached input, output, and reasoning tokens. A low cache-read dollar estimate
        should not be read as total fast-mode consumption.
      </AccountingNote>
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

function PricingPage({
  pricing,
  usageRows
}: {
  pricing: PricingRecord[];
  usageRows: NamedUsagePoint[];
}) {
  const usageByModel = useMemo(
    () => new Map(usageRows.map((row) => [row.name, row])),
    [usageRows]
  );
  const maxUsage = Math.max(1, ...usageRows.map((row) => row.total_tokens));
  const maxCost = Math.max(0.01, ...usageRows.map((row) => row.estimated_cost_usd));
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
              <th>Usage</th>
              <th>Input</th>
              <th>Output</th>
              <th>Cache read</th>
              <th>Cache write</th>
              <th>Source</th>
            </tr>
          </thead>
          <tbody>
            {pricing.map((row) => {
              const usage = usageByModel.get(row.model);
              return (
                <tr key={`${row.provider}-${row.model}`}>
                  <td>{row.provider}</td>
                  <td>{row.model}</td>
                  <td>
                    {usage ? (
                      <div className="pricing-usage">
                        <UsageBar row={usage} max={maxUsage} compactMode />
                        <UsageSummary row={usage} />
                        <UsageBar row={usage} max={maxCost} metric="cost" compactMode />
                        <UsageSummary row={usage} metric="cost" />
                      </div>
                    ) : (
                      <span className="raw-path">not imported</span>
                    )}
                  </td>
                  <td>{money(row.input_rate)}</td>
                  <td>{money(row.output_rate)}</td>
                  <td>{money(row.cache_read_rate)}</td>
                  <td>{money(row.cache_write_rate)}</td>
                  <td>
                    <Status
                      value={row.local_free_flag ? "free" : row.override_flag ? "override" : "bundled"}
                      tone={row.override_flag || row.local_free_flag ? "info" : "neutral"}
                    />
                    <span className="pricing-version">
                      {row.source_label} / {row.provider}/{row.model} / {row.snapshot_version}
                    </span>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      <p className="table-note">
        Rates are per 1M tokens for cost estimation. Cache-read pricing is not the same thing as
        Codex fast-mode subscription usage.
      </p>
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

function TrendPanel({
  title,
  rows,
  className = "",
  metric = "tokens"
}: {
  title: string;
  rows: NamedUsagePoint[];
  className?: string;
  metric?: UsageBarMetric;
}) {
  const max = Math.max(metric === "cost" ? 0.01 : 1, ...rows.map((row) => barValue(row, metric)));
  const hasPriority = rows.some((row) => priorityBarValue(row, metric) > 0);
  return (
    <section className={`panel ${className}`}>
      <div className="panel-header">
        <h2>{title}</h2>
        <span>{hasPriority ? `${rows.length} rows, yellow is fast/priority` : `${rows.length} rows`}</span>
      </div>
      <div className="bar-list">
        {rows.length === 0 ? <Empty text="No imported usage yet." /> : null}
        {rows.map((row) => (
          <div className="bar-row" key={row.name}>
            <div className="bar-label">
              <span>{row.name || "unknown"}</span>
              <small>{money(row.estimated_cost_usd)}</small>
            </div>
            <UsageBar row={row} max={max} metric={metric} />
            <UsageSummary row={row} metric={metric} />
          </div>
        ))}
      </div>
    </section>
  );
}

function UsageBar({
  row,
  max,
  metric = "tokens",
  compactMode = false
}: {
  row: NamedUsagePoint;
  max: number;
  metric?: UsageBarMetric;
  compactMode?: boolean;
}) {
  const value = barValue(row, metric);
  const priorityValue = priorityBarValue(row, metric);
  const fill = value > 0 ? Math.max(compactMode ? 3 : 4, (value / max) * 100) : 0;
  const priorityWidth = value > 0 ? Math.min(fill, fill * (priorityValue / value)) : 0;
  const priorityLeft = Math.max(0, fill - priorityWidth);
  const totalLabel = metric === "cost" ? money(value) : `${compact(value)} tokens`;
  const priorityLabel =
    metric === "cost" ? money(priorityValue) : `${compact(priorityValue)} priority/fast tokens`;
  const label =
    priorityValue > 0
      ? `${row.name || "unknown"}: ${totalLabel}, ${priorityLabel}`
      : `${row.name || "unknown"}: ${totalLabel}`;

  return (
    <div
      className={compactMode ? "bar-track compact" : "bar-track"}
      data-metric={metric}
      role="img"
      aria-label={label}
      title={label}
    >
      <span className="bar-fill" style={{ width: `${fill}%` }} />
      {priorityValue > 0 ? (
        <span
          className="bar-priority"
          style={{ left: `${priorityLeft}%`, width: `${priorityWidth}%` }}
        />
      ) : null}
    </div>
  );
}

function UsageSummary({ row, metric = "tokens" }: { row: NamedUsagePoint; metric?: UsageBarMetric }) {
  const inputWithCache = row.prompt_tokens + row.cache_read_tokens + row.cache_write_tokens;
  const generated = row.completion_tokens + row.reasoning_tokens;
  if (metric === "cost") {
    return (
      <small className="token-summary">
        <span>{money(row.estimated_cost_usd)} total</span>
        {row.priority_estimated_cost_usd > 0 ? (
          <span className="fast-label" title={`${money(row.priority_estimated_cost_usd)} priority/fast spend`}>
            {money(row.priority_estimated_cost_usd)} fast
          </span>
        ) : null}
      </small>
    );
  }
  return (
    <small className="token-summary">
      <span>{compact(row.total_tokens)} tokens</span>
      <span title={`${compact(inputWithCache)} input tokens including cached reads`}>
        {compact(inputWithCache)} input
      </span>
      <span title={`${compact(generated)} output tokens`}>
        {compact(generated)} output
      </span>
      {row.priority_tokens > 0 ? (
        <span className="fast-label" title={`${compact(row.priority_tokens)} priority/fast tokens`}>
          {compact(row.priority_tokens)} fast
        </span>
      ) : null}
    </small>
  );
}

function barValue(row: NamedUsagePoint, metric: UsageBarMetric) {
  return metric === "cost" ? row.estimated_cost_usd : row.total_tokens;
}

function priorityBarValue(row: NamedUsagePoint, metric: UsageBarMetric) {
  return metric === "cost" ? row.priority_estimated_cost_usd : row.priority_tokens;
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

function AccountingNote({
  title,
  badge,
  children
}: {
  title: string;
  badge: string;
  children: React.ReactNode;
}) {
  return (
    <section className="panel note-panel wide">
      <div className="panel-header">
        <h2>{title}</h2>
        <span>{badge}</span>
      </div>
      <p>{children}</p>
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
