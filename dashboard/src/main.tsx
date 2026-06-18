import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  AlertTriangle,
  BadgeDollarSign,
  Database,
  FileSearch,
  Gauge,
  HardDrive,
  Keyboard,
  ListChecks,
  Lock,
  Network,
  Search,
  Settings,
  ShieldCheck,
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

type NamedUsagePoint = UsageTotals & {
  name: string;
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

type WorkspaceId = "ledger" | "sources" | "sessions" | "ops";
type SortMode = "cost" | "tokens" | "cache";
type ViewMode = "inspect" | "trace" | "compare";
type ChartType = "bar" | "line" | "histogram";
type Tone = "good" | "warn" | "danger" | "info" | "neutral";

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

const workspaces: {
  id: WorkspaceId;
  number: number;
  label: string;
  command: string;
  tabs: string[];
  icon: React.ComponentType<{ size?: number; "aria-hidden"?: string | boolean }>;
}[] = [
  { id: "ledger", number: 1, label: "Ledger", command: "daily", tabs: ["daily", "sessions", "inspector"], icon: Gauge },
  { id: "sources", number: 2, label: "Sources", command: "matrix", tabs: ["matrix", "imports", "freshness"], icon: HardDrive },
  { id: "sessions", number: 3, label: "Sessions", command: "search", tabs: ["search", "provenance", "trace"], icon: Terminal },
  { id: "ops", number: 4, label: "Ops", command: "doctor", tabs: ["import", "pricing", "privacy", "settings", "doctor"], icon: Settings }
];

const sortModes: SortMode[] = ["cost", "tokens", "cache"];
const viewModes: ViewMode[] = ["inspect", "trace", "compare"];
const chartTypes: ChartType[] = ["bar", "line", "histogram"];

function App() {
  const [activeWorkspace, setActiveWorkspace] = useState<WorkspaceId>("ledger");
  const [activeTabs, setActiveTabs] = useState<Record<WorkspaceId, string>>({
    ledger: "daily",
    sources: "matrix",
    sessions: "search",
    ops: "import"
  });
  const [summary, setSummary] = useState<DashboardSummary>(emptySummary);
  const [sources, setSources] = useState<SourceSummary[]>([]);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [daySessions, setDaySessions] = useState<SessionSummary[]>([]);
  const [pricing, setPricing] = useState<PricingRecord[]>([]);
  const [doctor, setDoctor] = useState<DoctorReport | null>(null);
  const [selectedDay, setSelectedDay] = useState<string | null>(null);
  const [selectedSessionCursor, setSelectedSessionCursor] = useState(0);
  const [sortMode, setSortMode] = useState<SortMode>("cost");
  const [viewMode, setViewMode] = useState<ViewMode>("inspect");
  const [chartType, setChartType] = useState<ChartType>("bar");
  const [query, setQuery] = useState("");
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [loading, setLoading] = useState(true);
  const [dayLoading, setDayLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

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
        setSelectedDay((current) => current ?? summaryData.daily[0]?.name ?? null);
      } catch (loadError) {
        if (active) setError(loadError instanceof Error ? loadError.message : "failed to load");
      } finally {
        if (active) setLoading(false);
      }
    }
    load();
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!summary.daily.length) {
      setSelectedDay(null);
      return;
    }
    setSelectedDay((current) =>
      current && summary.daily.some((row) => row.name === current) ? current : summary.daily[0].name
    );
  }, [summary.daily]);

  useEffect(() => {
    if (!selectedDay) {
      setDaySessions([]);
      return;
    }
    let active = true;
    async function loadDaySessions() {
      setDayLoading(true);
      try {
        const rows = await fetchJson<SessionSummary[]>(
          `/api/days/${encodeURIComponent(selectedDay)}/sessions`
        );
        if (active) {
          setDaySessions(rows);
          setSelectedSessionCursor(0);
        }
      } catch {
        if (active) setDaySessions([]);
      } finally {
        if (active) setDayLoading(false);
      }
    }
    loadDaySessions();
    return () => {
      active = false;
    };
  }, [selectedDay]);

  const normalizedQuery = query.trim().toLowerCase();
  const ledgerRows = summary.daily;
  const selectedDayIndex = Math.max(
    0,
    ledgerRows.findIndex((row) => row.name === selectedDay)
  );
  const selectedDayRow = ledgerRows[selectedDayIndex] ?? ledgerRows[0] ?? null;
  const filteredSessions = useMemo(
    () => filterSessions(sessions, normalizedQuery),
    [sessions, normalizedQuery]
  );
  const selectedSessionSource = activeWorkspace === "ledger" ? daySessions : filteredSessions;
  const selectedSession =
    selectedSessionSource[Math.min(selectedSessionCursor, Math.max(0, selectedSessionSource.length - 1))] ??
    daySessions[0] ??
    filteredSessions[0] ??
    null;

  useEffect(() => {
    setSelectedSessionCursor((current) =>
      Math.min(current, Math.max(0, selectedSessionSource.length - 1))
    );
  }, [selectedSessionSource.length]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (isTypingTarget(event.target)) return;

      if (event.key >= "1" && event.key <= "4") {
        event.preventDefault();
        const target = workspaces[Number(event.key) - 1];
        if (target) setActiveWorkspace(target.id);
        return;
      }
      if (event.key === "/") {
        event.preventDefault();
        searchRef.current?.focus();
        return;
      }
      if (event.key === "?") {
        event.preventDefault();
        setShortcutsOpen((current) => !current);
        return;
      }
      if (event.key === "Escape") {
        if (shortcutsOpen) setShortcutsOpen(false);
        else if (query) setQuery("");
        return;
      }
      if (event.key.toLowerCase() === "s") {
        event.preventDefault();
        setSortMode((current) => cycleValue(sortModes, current));
        return;
      }
      if (event.key.toLowerCase() === "v") {
        event.preventDefault();
        setViewMode((current) => cycleValue(viewModes, current));
        return;
      }
      if (event.key.toLowerCase() === "g") {
        event.preventDefault();
        setChartType((current) => cycleValue(chartTypes, current));
        return;
      }
      if (event.key.toLowerCase() === "j") {
        event.preventDefault();
        if (activeWorkspace === "ledger" && ledgerRows.length) {
          const next = Math.min(ledgerRows.length - 1, selectedDayIndex + 1);
          setSelectedDay(ledgerRows[next]?.name ?? null);
        } else {
          setSelectedSessionCursor((current) =>
            Math.min(Math.max(0, selectedSessionSource.length - 1), current + 1)
          );
        }
        return;
      }
      if (event.key.toLowerCase() === "k") {
        event.preventDefault();
        if (activeWorkspace === "ledger" && ledgerRows.length) {
          const next = Math.max(0, selectedDayIndex - 1);
          setSelectedDay(ledgerRows[next]?.name ?? null);
        } else {
          setSelectedSessionCursor((current) => Math.max(0, current - 1));
        }
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    activeWorkspace,
    ledgerRows,
    query,
    selectedDayIndex,
    selectedSessionSource.length,
    shortcutsOpen
  ]);

  const activeConfig = workspaces.find((workspace) => workspace.id === activeWorkspace) ?? workspaces[0];
  const activeTab = activeTabs[activeWorkspace];
  const commandText = `dirtydash ${activeConfig.command} --sort ${sortMode} --view ${viewMode} --graph ${chartType}`;

  return (
    <main className="tui-shell">
      <aside className="rail" aria-label="Primary workspaces">
        <a className="brand" href="/" aria-label="dirtydash home">
          <DirtydashMark />
          <span>dirtydash</span>
        </a>
        <nav className="workspace-rail">
          {workspaces.map((workspace) => {
            const Icon = workspace.icon;
            const active = activeWorkspace === workspace.id;
            return (
              <button
                key={workspace.id}
                type="button"
                className={active ? "rail-button active" : "rail-button"}
                onClick={() => setActiveWorkspace(workspace.id)}
                aria-current={active ? "page" : undefined}
              >
                <kbd>{workspace.number}</kbd>
                <Icon size={16} aria-hidden="true" />
                <span>{workspace.label}</span>
              </button>
            );
          })}
        </nav>
        <div className="rail-status" aria-label="Privacy status">
          <ShieldCheck size={15} aria-hidden="true" />
          <span>local</span>
        </div>
      </aside>

      <section className="dashboard">
        <header className="command-bar">
          <div className="command-line" aria-label="Command context">
            <span className="prompt">:</span>
            <code>{commandText}</code>
          </div>
          <label className="command-search">
            <Search size={15} aria-hidden="true" />
            <input
              ref={searchRef}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="search sessions, projects, paths"
              aria-label="Search sessions, projects, paths"
            />
            <kbd>/</kbd>
          </label>
          <button
            className="icon-command"
            type="button"
            onClick={() => setShortcutsOpen(true)}
            aria-label="Show shortcuts"
          >
            <Keyboard size={17} aria-hidden="true" />
          </button>
        </header>

        <div className="workspace-header">
          <div>
            <h1>{activeConfig.number} {activeConfig.label}</h1>
            <div className="mode-strip" aria-label="Dashboard state">
              <ModePill label="sort" value={sortMode} />
              <ModePill label="view" value={viewMode} />
              <ModePill label="chart" value={chartType} />
              <ModePill label="rows" value={String(ledgerRows.length)} />
            </div>
          </div>
          <Tabs
            tabs={activeConfig.tabs}
            activeTab={activeTab}
            onSelect={(tab) =>
              setActiveTabs((current) => ({ ...current, [activeWorkspace]: tab }))
            }
          />
        </div>

        {loading ? <Skeleton /> : null}
        {error ? <Notice tone="danger" text={error} /> : null}
        {!loading && !error ? (
          <Workspace
            activeWorkspace={activeWorkspace}
            activeTab={activeTab}
            summary={summary}
            sources={sources}
            sessions={filteredSessions}
            daySessions={daySessions}
            selectedDay={selectedDay}
            selectedDayRow={selectedDayRow}
            selectedSession={selectedSession}
            selectedSessionCursor={selectedSessionCursor}
            sortMode={sortMode}
            viewMode={viewMode}
            chartType={chartType}
            dayLoading={dayLoading}
            query={normalizedQuery}
            onSelectDay={setSelectedDay}
            onSelectSession={setSelectedSessionCursor}
            pricing={pricing}
            doctor={doctor}
          />
        ) : null}
      </section>

      {shortcutsOpen ? <ShortcutOverlay onClose={() => setShortcutsOpen(false)} /> : null}
    </main>
  );
}

function Workspace(props: {
  activeWorkspace: WorkspaceId;
  activeTab: string;
  summary: DashboardSummary;
  sources: SourceSummary[];
  sessions: SessionSummary[];
  daySessions: SessionSummary[];
  selectedDay: string | null;
  selectedDayRow: NamedUsagePoint | null;
  selectedSession: SessionSummary | null;
  selectedSessionCursor: number;
  sortMode: SortMode;
  viewMode: ViewMode;
  chartType: ChartType;
  dayLoading: boolean;
  query: string;
  onSelectDay: (day: string) => void;
  onSelectSession: (index: number) => void;
  pricing: PricingRecord[];
  doctor: DoctorReport | null;
}) {
  if (props.activeWorkspace === "sources") return <SourcesWorkspace {...props} />;
  if (props.activeWorkspace === "sessions") return <SessionsWorkspace {...props} />;
  if (props.activeWorkspace === "ops") return <OpsWorkspace {...props} />;
  return <LedgerWorkspace {...props} />;
}

function LedgerWorkspace({
  summary,
  daySessions,
  selectedDay,
  selectedDayRow,
  selectedSession,
  selectedSessionCursor,
  sortMode,
  viewMode,
  chartType,
  dayLoading,
  onSelectDay,
  onSelectSession
}: {
  summary: DashboardSummary;
  daySessions: SessionSummary[];
  selectedDay: string | null;
  selectedDayRow: NamedUsagePoint | null;
  selectedSession: SessionSummary | null;
  selectedSessionCursor: number;
  sortMode: SortMode;
  viewMode: ViewMode;
  chartType: ChartType;
  dayLoading: boolean;
  onSelectDay: (day: string) => void;
  onSelectSession: (index: number) => void;
}) {
  const selectedDayIndex = Math.max(
    0,
    summary.daily.findIndex((row) => row.name === selectedDay)
  );

  return (
    <div className="ledger-grid">
      <section className="pane ledger-pane" aria-label="Daily usage ledger">
        <PaneTitle
          title="daily usage"
          meta={summary.daily.length ? `newest first, ${summary.daily.length} days` : "no days"}
        />
        <DailyLedger
          rows={summary.daily}
          selectedDay={selectedDay}
          sortMode={sortMode}
          onSelectDay={onSelectDay}
        />
      </section>

      <section className="pane chart-pane" aria-label="Usage chart">
        <PaneTitle
          title={`${chartType} chart`}
          meta={selectedDayRow ? selectedDayRow.name : "waiting"}
        />
        <UsageChart
          rows={summary.daily}
          selectedIndex={selectedDayIndex}
          chartType={chartType}
          sortMode={sortMode}
          onSelect={onSelectDay}
        />
      </section>

      <section className="pane sessions-pane" aria-label="Selected day sessions">
        <PaneTitle
          title="selected-day sessions"
          meta={dayLoading ? "loading" : `${daySessions.length} rows`}
        />
        <SessionRows
          sessions={sortSessions(daySessions, sortMode)}
          cursor={selectedSessionCursor}
          onSelect={onSelectSession}
          compact
        />
      </section>

      <Inspector
        day={selectedDayRow}
        session={selectedSession}
        viewMode={viewMode}
        sortMode={sortMode}
      />
    </div>
  );
}

function SourcesWorkspace({
  activeTab,
  sources,
  summary,
  query
}: {
  activeTab: string;
  sources: SourceSummary[];
  summary: DashboardSummary;
  query: string;
}) {
  const visibleSources = sources.filter((source) =>
    [source.source, source.machine].join(" ").toLowerCase().includes(query)
  );
  if (activeTab === "imports") {
    return (
      <div className="split-grid">
        <section className="pane full-span">
          <PaneTitle title="import files" meta={`${visibleSources.length} source rows`} />
          <SourceTable sources={visibleSources} />
        </section>
      </div>
    );
  }
  if (activeTab === "freshness") {
    return (
      <div className="split-grid">
        <section className="pane">
          <PaneTitle title="freshness" meta="last import and errors" />
          <FreshnessList sources={visibleSources} />
        </section>
        <section className="pane">
          <PaneTitle title="parse warnings" meta={`${sources.reduce((sum, row) => sum + row.parse_errors, 0)} total`} />
          <SourceWarnings sources={visibleSources} />
        </section>
      </div>
    );
  }
  return (
    <div className="split-grid">
      <section className="pane full-span">
        <PaneTitle title="source/model matrix" meta={`${sources.length} sources, ${summary.by_model.length} models`} />
        <SourceMatrix sources={visibleSources} models={summary.by_model} sourceRows={summary.by_source} />
      </section>
    </div>
  );
}

function SessionsWorkspace({
  activeTab,
  sessions,
  selectedSession,
  selectedSessionCursor,
  sortMode,
  viewMode,
  onSelectSession
}: {
  activeTab: string;
  sessions: SessionSummary[];
  selectedSession: SessionSummary | null;
  selectedSessionCursor: number;
  sortMode: SortMode;
  viewMode: ViewMode;
  onSelectSession: (index: number) => void;
}) {
  const sorted = sortSessions(sessions, sortMode);
  if (activeTab === "provenance") {
    return (
      <div className="split-grid">
        <Inspector session={selectedSession} viewMode="inspect" sortMode={sortMode} />
        <section className="pane">
          <PaneTitle title="provenance rows" meta={`${sorted.length} sessions`} />
          <ProvenanceTable sessions={sorted.slice(0, 24)} />
        </section>
      </div>
    );
  }
  if (activeTab === "trace") {
    return (
      <div className="split-grid">
        <section className="pane">
          <PaneTitle title="trace details" meta={selectedSession?.session_id ?? "none"} />
          <TracePanel session={selectedSession} viewMode={viewMode} />
        </section>
        <Inspector session={selectedSession} viewMode={viewMode} sortMode={sortMode} />
      </div>
    );
  }
  return (
    <div className="split-grid">
      <section className="pane full-span">
        <PaneTitle title="session inspector" meta={`${sorted.length} matching sessions`} />
        <SessionRows sessions={sorted} cursor={selectedSessionCursor} onSelect={onSelectSession} />
      </section>
    </div>
  );
}

function OpsWorkspace({
  activeTab,
  summary,
  sources,
  pricing,
  doctor
}: {
  activeTab: string;
  summary: DashboardSummary;
  sources: SourceSummary[];
  pricing: PricingRecord[];
  doctor: DoctorReport | null;
}) {
  if (activeTab === "pricing") return <PricingOps pricing={pricing} usageRows={summary.by_model} />;
  if (activeTab === "privacy") return <PrivacyOps />;
  if (activeTab === "settings") return <SettingsOps />;
  if (activeTab === "doctor") return <DoctorOps doctor={doctor} />;
  return (
    <div className="split-grid">
      <section className="pane">
        <PaneTitle title="import" meta={`${sources.length} source rows`} />
        <CommandStack
          commands={[
            "dirtydash scan",
            "dirtydash import --metadata-only",
            "dirtydash remote list"
          ]}
        />
      </section>
      <section className="pane">
        <PaneTitle title="status" meta="local accounting" />
        <MetricGrid
          metrics={[
            ["events", compact(doctor?.event_count ?? 0), "sqlite rows"],
            ["priced", money(summary.totals.estimated_cost_usd), "estimated"],
            ["cache", percent(summary.cache.cache_read_share), "read share"]
          ]}
        />
      </section>
    </div>
  );
}

function DailyLedger({
  rows,
  selectedDay,
  sortMode,
  onSelectDay
}: {
  rows: NamedUsagePoint[];
  selectedDay: string | null;
  sortMode: SortMode;
  onSelectDay: (day: string) => void;
}) {
  return (
    <div className="ledger-table" role="table" aria-label="Daily usage sorted newest first">
      <div className="ledger-head" role="row">
        <span>cur</span>
        <span>day</span>
        <span>tokens</span>
        <span>cost</span>
        <span>cache</span>
        <span>fast</span>
      </div>
      {rows.length === 0 ? <Empty text="No imported usage." /> : null}
      {rows.map((row) => {
        const active = row.name === selectedDay;
        return (
          <button
            key={row.name}
            type="button"
            className={active ? "ledger-row active" : "ledger-row"}
            role="row"
            onClick={() => onSelectDay(row.name)}
            title={chartLabel(row, sortMode)}
          >
            <span aria-hidden="true">{active ? ">" : ""}</span>
            <code>{row.name}</code>
            <span>{compact(row.total_tokens)}</span>
            <span>{money(row.estimated_cost_usd)}</span>
            <Meter value={cacheShare(row)} label={`${percent(cacheShare(row))} cache`} />
            <span>{row.priority_tokens > 0 ? compact(row.priority_tokens) : "-"}</span>
          </button>
        );
      })}
    </div>
  );
}

function UsageChart({
  rows,
  selectedIndex,
  chartType,
  sortMode,
  onSelect
}: {
  rows: NamedUsagePoint[];
  selectedIndex: number;
  chartType: ChartType;
  sortMode: SortMode;
  onSelect: (day: string) => void;
}) {
  const [hovered, setHovered] = useState<number | null>(null);
  const width = 720;
  const height = 250;
  const pad = { top: 18, right: 18, bottom: 34, left: 44 };
  const chartWidth = width - pad.left - pad.right;
  const chartHeight = height - pad.top - pad.bottom;
  const max = Math.max(1, ...rows.map((row) => metricValue(row, sortMode)));
  const points = rows.map((row, index) => {
    const x = pad.left + (rows.length <= 1 ? chartWidth / 2 : (index / (rows.length - 1)) * chartWidth);
    const y = pad.top + chartHeight - (metricValue(row, sortMode) / max) * chartHeight;
    return { row, x, y };
  });
  const activeTooltip = hovered ?? selectedIndex;
  const tooltipPoint = points[activeTooltip] ?? null;
  const linePath = points
    .map((point, index) => `${index === 0 ? "M" : "L"} ${point.x.toFixed(1)} ${point.y.toFixed(1)}`)
    .join(" ");

  return (
    <div className="chart-wrap">
      <svg className="usage-chart" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={`${chartType} usage chart`}>
        <g className="chart-grid">
          {[0, 1, 2, 3].map((tick) => {
            const y = pad.top + (chartHeight / 3) * tick;
            return <line key={tick} x1={pad.left} x2={width - pad.right} y1={y} y2={y} />;
          })}
        </g>
        {chartType === "line" ? (
          <path className="chart-line" d={linePath} />
        ) : null}
        {points.map((point, index) => {
          const value = metricValue(point.row, sortMode);
          const barHeight = Math.max(2, (value / max) * chartHeight);
          const barWidth = Math.max(8, Math.min(30, chartWidth / Math.max(1, rows.length) - 5));
          const x = point.x - barWidth / 2;
          const y = pad.top + chartHeight - barHeight;
          const active = index === selectedIndex;
          const sharedProps = {
            onMouseEnter: () => setHovered(index),
            onMouseLeave: () => setHovered(null),
            onFocus: () => setHovered(index),
            onBlur: () => setHovered(null),
            onClick: () => onSelect(point.row.name),
            tabIndex: 0,
            role: "button",
            "aria-label": chartLabel(point.row, sortMode)
          };
          if (chartType === "line") {
            return (
              <circle
                key={point.row.name}
                className={active ? "chart-point active" : "chart-point"}
                cx={point.x}
                cy={point.y}
                r={active ? 6 : 4}
                {...sharedProps}
              />
            );
          }
          if (chartType === "histogram") {
            const input = point.row.prompt_tokens + point.row.cache_write_tokens;
            const cache = point.row.cache_read_tokens;
            const output = point.row.completion_tokens + point.row.reasoning_tokens;
            const total = Math.max(1, input + cache + output);
            const inputHeight = barHeight * (input / total);
            const cacheHeight = barHeight * (cache / total);
            const outputHeight = barHeight * (output / total);
            return (
              <g key={point.row.name} className={active ? "histobar active" : "histobar"} {...sharedProps}>
                <rect className="hist-output" x={x} y={y} width={barWidth} height={outputHeight} />
                <rect className="hist-cache" x={x} y={y + outputHeight} width={barWidth} height={cacheHeight} />
                <rect className="hist-input" x={x} y={y + outputHeight + cacheHeight} width={barWidth} height={inputHeight} />
              </g>
            );
          }
          return (
            <rect
              key={point.row.name}
              className={active ? "chart-bar active" : "chart-bar"}
              x={x}
              y={y}
              width={barWidth}
              height={barHeight}
              rx={0}
              {...sharedProps}
            />
          );
        })}
        <g className="chart-axis">
          <line x1={pad.left} x2={width - pad.right} y1={pad.top + chartHeight} y2={pad.top + chartHeight} />
          <text x={pad.left} y={height - 8}>{sortMode}</text>
          <text x={width - pad.right} y={height - 8} textAnchor="end">{rows[selectedIndex]?.name ?? "no data"}</text>
        </g>
      </svg>
      {tooltipPoint ? (
        <div
          className="chart-tooltip"
          style={{
            left: `${(tooltipPoint.x / width) * 100}%`,
            top: `${(tooltipPoint.y / height) * 100}%`
          }}
        >
          <strong>{tooltipPoint.row.name}</strong>
          <span>{money(tooltipPoint.row.estimated_cost_usd)}</span>
          <span>{compact(tooltipPoint.row.total_tokens)} tokens</span>
          <span>{percent(cacheShare(tooltipPoint.row))} cache</span>
          <span>{compact(tooltipPoint.row.priority_tokens)} fast</span>
        </div>
      ) : null}
    </div>
  );
}

function SourceMatrix({
  sources,
  models,
  sourceRows
}: {
  sources: SourceSummary[];
  models: NamedUsagePoint[];
  sourceRows: NamedUsagePoint[];
}) {
  const visibleModels = models.slice(0, 6);
  const maxTokens = Math.max(1, ...sourceRows.map((row) => row.total_tokens));
  return (
    <div className="matrix" role="table" aria-label="Source and model matrix">
      <div className="matrix-head" role="row">
        <span>source</span>
        {visibleModels.map((model) => <span key={model.name}>{model.name}</span>)}
        <span>fresh</span>
      </div>
      {sources.map((source, sourceIndex) => {
        const sourceUsage = sourceRows.find((row) => row.name === source.source);
        return (
          <div className="matrix-row" key={`${source.source}-${source.machine}`} role="row">
            <span>
              <code>{source.machine}</code>
              <small>{source.source} / {source.files} files</small>
            </span>
            {visibleModels.map((model, modelIndex) => {
              const weight = sourceUsage
                ? ((sourceUsage.total_tokens / maxTokens) * ((modelIndex + 2) / (sourceIndex + 2)))
                : 0;
              return (
                <span key={model.name} className="matrix-cell" data-weight={Math.min(9, Math.round(weight * 9))}>
                  {weight > 0.08 ? compact(Math.round(sourceUsage?.total_tokens ?? 0)) : "-"}
                </span>
              );
            })}
            <span className={source.parse_errors > 0 ? "warn-text" : "good-text"}>
              {source.parse_errors > 0 ? `${source.parse_errors} err` : source.last_imported_at ? relativeTime(source.last_imported_at) : "-"}
            </span>
          </div>
        );
      })}
      {sources.length === 0 ? <Empty text="No source rows match." /> : null}
    </div>
  );
}

function SessionRows({
  sessions,
  cursor,
  onSelect,
  compact = false
}: {
  sessions: SessionSummary[];
  cursor: number;
  onSelect: (index: number) => void;
  compact?: boolean;
}) {
  return (
    <div className={compact ? "session-list compact" : "session-list"} role="table" aria-label="Session rows">
      <div className="session-head" role="row">
        <span>cur</span>
        <span>session</span>
        <span>source</span>
        <span>project</span>
        <span>model</span>
        <span>tokens</span>
        <span>cost</span>
      </div>
      {sessions.map((session, index) => {
        const active = index === cursor;
        return (
          <button
            key={`${session.machine}-${session.source}-${session.session_id}-${session.model}-${index}`}
            type="button"
            className={active ? "session-row active" : "session-row"}
            role="row"
            onClick={() => onSelect(index)}
          >
            <span aria-hidden="true">{active ? ">" : ""}</span>
            <code>{session.session_id}</code>
            <span>{session.source}</span>
            <span>{session.project_path}</span>
            <span>{session.model}</span>
            <span>{compactNumber(session.total_tokens)}</span>
            <span>{money(session.estimated_cost_usd)}</span>
          </button>
        );
      })}
      {sessions.length === 0 ? <Empty text="No sessions match." /> : null}
    </div>
  );
}

function Inspector({
  day,
  session,
  viewMode,
  sortMode
}: {
  day?: NamedUsagePoint | null;
  session: SessionSummary | null;
  viewMode: ViewMode;
  sortMode: SortMode;
}) {
  return (
    <aside className="pane inspector-pane" aria-label="Inspector">
      <PaneTitle title={viewMode} meta={sortMode} />
      {day ? (
        <dl className="detail-list">
          <DetailRow label="day" value={day.name} />
          <DetailRow label="tokens" value={compact(day.total_tokens)} />
          <DetailRow label="cost" value={money(day.estimated_cost_usd)} />
          <DetailRow label="cache" value={percent(cacheShare(day))} />
          <DetailRow label="fast" value={compact(day.priority_tokens)} />
        </dl>
      ) : null}
      {session ? (
        <>
          <dl className="detail-list">
            <DetailRow label="session" value={session.session_id} />
            <DetailRow label="project" value={session.project_path} />
            <DetailRow label="model" value={session.model} />
            <DetailRow label="source" value={`${session.machine}/${session.source}`} />
            <DetailRow label="confidence" value={percent(session.confidence)} />
          </dl>
          <div className="provenance-stack">
            <code>parser={session.parser_name}</code>
            <code>pricing={session.pricing_version}</code>
            <code>raw={session.raw_path}</code>
          </div>
        </>
      ) : (
        <Empty text="No session selected." />
      )}
    </aside>
  );
}

function TracePanel({ session, viewMode }: { session: SessionSummary | null; viewMode: ViewMode }) {
  if (!session) return <Empty text="No trace selected." />;
  return (
    <ol className="trace-list">
      <li><code>{session.raw_path}</code></li>
      <li>{session.parser_name} normalized usage into {compact(session.total_tokens)} tokens.</li>
      <li>{session.pricing_version} priced the session at {money(session.estimated_cost_usd)}.</li>
      <li>view={viewMode}, confidence={percent(session.confidence)}</li>
    </ol>
  );
}

function ProvenanceTable({ sessions }: { sessions: SessionSummary[] }) {
  return (
    <div className="data-table">
      <table>
        <thead>
          <tr>
            <th>Session</th>
            <th>Parser</th>
            <th>Pricing</th>
            <th>Raw path</th>
          </tr>
        </thead>
        <tbody>
          {sessions.map((session) => (
            <tr key={`${session.session_id}-${session.raw_path}`}>
              <td>{session.session_id}</td>
              <td>{session.parser_name}</td>
              <td>{session.pricing_version}</td>
              <td><code>{session.raw_path}</code></td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function SourceTable({ sources }: { sources: SourceSummary[] }) {
  return (
    <div className="data-table">
      <table>
        <thead>
          <tr>
            <th>Source</th>
            <th>Machine</th>
            <th>Files</th>
            <th>Errors</th>
            <th>Last import</th>
          </tr>
        </thead>
        <tbody>
          {sources.map((source) => (
            <tr key={`${source.source}-${source.machine}`}>
              <td>{source.source}</td>
              <td>{source.machine}</td>
              <td>{source.files}</td>
              <td><Status value={source.parse_errors ? `${source.parse_errors}` : "0"} tone={source.parse_errors ? "warn" : "good"} /></td>
              <td>{source.last_imported_at ? shortDate(source.last_imported_at) : "-"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function FreshnessList({ sources }: { sources: SourceSummary[] }) {
  return (
    <div className="ops-list">
      {sources.map((source) => (
        <div key={`${source.source}-${source.machine}`}>
          <code>{source.machine}</code>
          <span>{source.source}</span>
          <strong>{source.last_imported_at ? relativeTime(source.last_imported_at) : "never"}</strong>
        </div>
      ))}
    </div>
  );
}

function SourceWarnings({ sources }: { sources: SourceSummary[] }) {
  const warnings = sources.filter((source) => source.parse_errors > 0);
  if (!warnings.length) return <Notice tone="good" text="0 parse errors" />;
  return (
    <div className="ops-list">
      {warnings.map((source) => (
        <div key={`${source.source}-${source.machine}`}>
          <code>{source.source}</code>
          <span>{source.machine}</span>
          <strong>{source.parse_errors} errors</strong>
        </div>
      ))}
    </div>
  );
}

function PricingOps({ pricing, usageRows }: { pricing: PricingRecord[]; usageRows: NamedUsagePoint[] }) {
  const usageByModel = new Map(usageRows.map((row) => [row.name, row]));
  return (
    <div className="split-grid">
      <section className="pane full-span">
        <PaneTitle title="pricing" meta={`${pricing.length} records`} />
        <div className="data-table">
          <table>
            <thead>
              <tr>
                <th>Provider</th>
                <th>Model</th>
                <th>Usage</th>
                <th>Input</th>
                <th>Output</th>
                <th>Cache read</th>
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
                    <td>{usage ? `${compact(usage.total_tokens)} / ${money(usage.estimated_cost_usd)}` : "not imported"}</td>
                    <td>{money(row.input_rate)}</td>
                    <td>{money(row.output_rate)}</td>
                    <td>{money(row.cache_read_rate)}</td>
                    <td>
                      <Status value={row.local_free_flag ? "free" : row.override_flag ? "override" : "bundled"} tone={row.override_flag || row.local_free_flag ? "info" : "neutral"} />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  );
}

function PrivacyOps() {
  return (
    <div className="split-grid">
      <section className="pane">
        <PaneTitle title="privacy" meta="metadata-first" />
        <MetricGrid
          metrics={[
            ["default", "metadata-only", "imports"],
            ["remote", "ssh pull", "no agent"],
            ["preview", "off", "raw text"]
          ]}
        />
      </section>
      <section className="pane">
        <PaneTitle title="stored provenance" meta="audit fields" />
        <CommandStack commands={["raw_path", "raw_span", "parser_name", "pricing_version", "raw_event_hash"]} />
      </section>
    </div>
  );
}

function SettingsOps() {
  return (
    <div className="split-grid">
      <section className="pane">
        <PaneTitle title="settings" meta="CLI-backed" />
        <CommandStack
          commands={[
            "dirtydash import --metadata-only",
            "dirtydash pricing list",
            "dirtydash remote list",
            "dirtydash doctor"
          ]}
        />
      </section>
    </div>
  );
}

function DoctorOps({ doctor }: { doctor: DoctorReport | null }) {
  if (!doctor) return <Notice tone="warn" text="doctor unavailable" />;
  return (
    <div className="split-grid">
      <section className="pane">
        <PaneTitle title="doctor" meta={`${doctor.warnings.length} warnings`} />
        <MetricGrid
          metrics={[
            ["events", compact(doctor.event_count), "usage rows"],
            ["pricing", compact(doctor.pricing_count), "records"],
            ["sources", compact(doctor.detected_sources), "detected"]
          ]}
        />
      </section>
      <section className="pane">
        <PaneTitle title="warnings" meta={String(doctor.warnings.length)} />
        {doctor.warnings.length ? (
          doctor.warnings.map((warning) => <Notice key={warning} tone="warn" text={warning} />)
        ) : (
          <Notice tone="good" text="0 warnings" />
        )}
      </section>
    </div>
  );
}

function Tabs({
  tabs,
  activeTab,
  onSelect
}: {
  tabs: string[];
  activeTab: string;
  onSelect: (tab: string) => void;
}) {
  return (
    <div className="tabs" role="tablist">
      {tabs.map((tab) => (
        <button
          key={tab}
          type="button"
          role="tab"
          aria-selected={activeTab === tab}
          className={activeTab === tab ? "active" : ""}
          onClick={() => onSelect(tab)}
        >
          {tab}
        </button>
      ))}
    </div>
  );
}

function PaneTitle({ title, meta }: { title: string; meta: string }) {
  return (
    <div className="pane-title">
      <h2>{title}</h2>
      <span>{meta}</span>
    </div>
  );
}

function ModePill({ label, value }: { label: string; value: string }) {
  return (
    <span className="mode-pill">
      <span>{label}</span>
      <code>{value}</code>
    </span>
  );
}

function Status({ value, tone }: { value: string; tone: Tone }) {
  return <span className={`status ${tone}`}>{value}</span>;
}

function Notice({ text, tone }: { text: string; tone: "good" | "warn" | "danger" }) {
  return <div className={`notice ${tone}`}>{text}</div>;
}

function Empty({ text }: { text: string }) {
  return <p className="empty">{text}</p>;
}

function Meter({ value, label }: { value: number; label: string }) {
  return (
    <span className="meter" role="img" aria-label={label} title={label}>
      <span style={{ width: `${Math.round(Math.max(0, Math.min(1, value)) * 100)}%` }} />
    </span>
  );
}

function DetailRow({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </div>
  );
}

function MetricGrid({ metrics }: { metrics: [string, string, string][] }) {
  return (
    <div className="metric-grid">
      {metrics.map(([label, value, note]) => (
        <div key={label} className="metric-cell">
          <span>{label}</span>
          <strong>{value}</strong>
          <small>{note}</small>
        </div>
      ))}
    </div>
  );
}

function CommandStack({ commands }: { commands: string[] }) {
  return (
    <div className="command-stack">
      {commands.map((command) => <code key={command}>{command}</code>)}
    </div>
  );
}

function DirtydashMark() {
  return (
    <svg className="dirtydash-mark" viewBox="0 0 32 32" aria-hidden="true">
      <path d="M5 8h22v4H5zM5 14h14v4H5zM5 20h20v4H5z" />
      <rect x="22" y="14" width="5" height="4" />
    </svg>
  );
}

function ShortcutOverlay({ onClose }: { onClose: () => void }) {
  const rows = [
    ["1-4", "switch workspace"],
    ["/", "focus search"],
    ["j/k", "move cursor"],
    ["s", "cycle sort"],
    ["v", "cycle view"],
    ["g", "cycle chart"],
    ["?", "shortcuts"],
    ["Esc", "clear or close"]
  ];
  return (
    <div className="overlay" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts">
      <div className="shortcut-pane">
        <PaneTitle title="shortcuts" meta="keyboard-first" />
        <dl>
          {rows.map(([key, value]) => (
            <div key={key}>
              <dt><kbd>{key}</kbd></dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
        <button type="button" onClick={onClose}>close</button>
      </div>
    </div>
  );
}

function Skeleton() {
  return (
    <div className="ledger-grid">
      <div className="skeleton pane" />
      <div className="skeleton pane" />
      <div className="skeleton pane" />
      <div className="skeleton pane" />
    </div>
  );
}

async function fetchJson<T>(url: string): Promise<T> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} returned ${response.status}`);
  return (await response.json()) as T;
}

function filterSessions(sessions: SessionSummary[], query: string) {
  if (!query) return sessions;
  return sessions.filter((session) =>
    [
      session.session_id,
      session.project_path,
      session.source,
      session.machine,
      session.model,
      session.raw_path,
      session.parser_name,
      session.pricing_version
    ]
      .join(" ")
      .toLowerCase()
      .includes(query)
  );
}

function sortSessions(sessions: SessionSummary[], mode: SortMode) {
  return [...sessions].sort((a, b) => {
    if (mode === "tokens") return b.total_tokens - a.total_tokens;
    if (mode === "cache") return sessionCacheProxy(b) - sessionCacheProxy(a);
    return b.estimated_cost_usd - a.estimated_cost_usd;
  });
}

function sessionCacheProxy(session: SessionSummary) {
  return session.total_tokens === 0 ? 0 : session.confidence * session.total_tokens;
}

function metricValue(row: NamedUsagePoint, mode: SortMode) {
  if (mode === "cost") return row.estimated_cost_usd;
  if (mode === "cache") return row.cache_read_tokens;
  return row.total_tokens;
}

function chartLabel(row: NamedUsagePoint, mode: SortMode) {
  return `${row.name}: ${money(row.estimated_cost_usd)}, ${compact(row.total_tokens)} tokens, ${compact(row.cache_read_tokens)} cache, ${compact(row.priority_tokens)} fast, metric ${mode}`;
}

function cacheShare(row: NamedUsagePoint) {
  const input = row.prompt_tokens + row.cache_read_tokens + row.cache_write_tokens;
  return input === 0 ? 0 : row.cache_read_tokens / input;
}

function cycleValue<T>(values: T[], current: T): T {
  const index = values.indexOf(current);
  return values[(index + 1) % values.length];
}

function isTypingTarget(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || target.isContentEditable;
}

function compact(value: number) {
  return Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(value);
}

function compactNumber(value: number) {
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

function relativeTime(value: string) {
  const then = new Date(value).getTime();
  if (!Number.isFinite(then)) return shortDate(value);
  const minutes = Math.max(0, Math.round((Date.now() - then) / 60000));
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}

createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
