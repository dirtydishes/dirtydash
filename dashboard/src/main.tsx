import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { MachinesWorkspace } from "./machines";
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
  by_reasoning_effort: NamedUsagePoint[];
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

type WorkspaceId = "ledger" | "sources" | "sessions" | "machines" | "ops";
type LedgerPaneId = "daily" | "chart" | "sessions" | "inspector";
type SortMode = "day" | "cost" | "tokens";
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
  by_reasoning_effort: [],
  by_project: [],
  expensive_sessions: []
};

const workspaces: {
  id: WorkspaceId;
  label: string;
  command: string;
  tabs: string[];
  icon: React.ComponentType<{ size?: number; "aria-hidden"?: string | boolean }>;
}[] = [
  { id: "ledger", label: "Ledger", command: "daily", tabs: ["daily", "sessions", "inspector"], icon: Gauge },
  { id: "sources", label: "Sources", command: "matrix", tabs: ["matrix", "imports", "freshness"], icon: HardDrive },
  { id: "sessions", label: "Sessions", command: "search", tabs: ["search", "provenance", "trace"], icon: Terminal },
  { id: "machines", label: "Machines", command: "machines", tabs: ["fleet", "enroll", "updates", "history"], icon: Network },
  { id: "ops", label: "Ops", command: "doctor", tabs: ["import", "pricing", "privacy", "settings", "doctor"], icon: Settings }
];

const sortModes: SortMode[] = ["day", "cost", "tokens"];
const viewModes: ViewMode[] = ["inspect", "trace", "compare"];
const chartTypes: ChartType[] = ["bar", "line", "histogram"];
const ledgerPaneOrder: LedgerPaneId[] = ["daily", "chart", "sessions", "inspector"];

function App() {
  const [activeWorkspace, setActiveWorkspace] = useState<WorkspaceId>("ledger");
  const [activeTabs, setActiveTabs] = useState<Record<WorkspaceId, string>>({
    ledger: "daily",
    sources: "matrix",
    sessions: "search",
    machines: "fleet",
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
  const [activePane, setActivePane] = useState<LedgerPaneId>("daily");
  const [sortMode, setSortMode] = useState<SortMode>("day");
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
            fetchJson<DashboardSummary>("/api/summary").catch(() => emptySummary),
            fetchJson<SourceSummary[]>("/api/sources").catch(() => []),
            fetchJson<SessionSummary[]>("/api/sessions").catch(() => []),
            fetchJson<PricingRecord[]>("/api/pricing").catch(() => []),
            fetchJson<DoctorReport>("/api/doctor").catch(() => null)
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
  const ledgerRows = useMemo(() => sortDailyRows(summary.daily, sortMode), [summary.daily, sortMode]);
  const selectedDayIndex = Math.max(
    0,
    ledgerRows.findIndex((row) => row.name === selectedDay)
  );
  const selectedDayRow = ledgerRows[selectedDayIndex] ?? ledgerRows[0] ?? null;
  const filteredSessions = useMemo(
    () => filterSessions(sessions, normalizedQuery),
    [sessions, normalizedQuery]
  );
  const sortedDaySessions = useMemo(
    () => sortSessions(daySessions, sortMode),
    [daySessions, sortMode]
  );
  const sortedFilteredSessions = useMemo(
    () => sortSessions(filteredSessions, sortMode),
    [filteredSessions, sortMode]
  );
  const selectedSessionSource = activeWorkspace === "ledger" ? sortedDaySessions : sortedFilteredSessions;
  const selectedSession =
    selectedSessionSource[Math.min(selectedSessionCursor, Math.max(0, selectedSessionSource.length - 1))] ??
    sortedDaySessions[0] ??
    sortedFilteredSessions[0] ??
    null;
  const activeConfig = workspaces.find((workspace) => workspace.id === activeWorkspace) ?? workspaces[0];
  const activeTab = activeTabs[activeWorkspace];

  useEffect(() => {
    setSelectedSessionCursor((current) =>
      Math.min(current, Math.max(0, selectedSessionSource.length - 1))
    );
  }, [selectedSessionSource.length]);

  useEffect(() => {
    if (activeWorkspace === "ledger") setActivePane("daily");
  }, [activeWorkspace]);

  useEffect(() => {
    if (activeWorkspace !== "ledger" || shortcutsOpen || loading) return;
    document
      .querySelector<HTMLElement>(`[data-pane="${activePane}"]`)
      ?.focus({ preventScroll: true });
  }, [activeWorkspace, activePane, loading, shortcutsOpen]);

  useEffect(() => {
    if (activeWorkspace !== "ledger") return;
    const selector =
      activePane === "daily"
        ? '[data-pane="daily"] .ledger-row.active'
        : activePane === "sessions"
          ? '[data-pane="sessions"] .session-row.active'
          : null;
    if (!selector) return;
    requestAnimationFrame(() => {
      document.querySelector<HTMLElement>(selector)?.scrollIntoView({
        block: "nearest",
        inline: "nearest"
      });
    });
  }, [activeWorkspace, activePane, selectedDay, selectedSessionCursor, selectedSessionSource.length]);

  useEffect(() => {
    function moveDay(delta: number) {
      if (!ledgerRows.length) return;
      const next = Math.max(0, Math.min(ledgerRows.length - 1, selectedDayIndex + delta));
      setSelectedDay(ledgerRows[next]?.name ?? null);
    }

    function moveSession(delta: number) {
      if (selectedSessionSource.length) {
        setSelectedSessionCursor((current) =>
          Math.max(0, Math.min(selectedSessionSource.length - 1, current + delta))
        );
      } else if (activeWorkspace === "ledger") {
        moveDay(delta);
      }
    }

    function moveTab(delta: number) {
      const tabs = (workspaces.find((workspace) => workspace.id === activeWorkspace) ?? workspaces[0]).tabs;
      const currentIndex = Math.max(0, tabs.indexOf(activeTab));
      const next = tabs[(currentIndex + delta + tabs.length) % tabs.length];
      if (next) {
        setActiveTabs((current) => ({ ...current, [activeWorkspace]: next }));
      }
    }

    function movePane(delta: number) {
      setActivePane((current) => cycleValueBy(ledgerPaneOrder, current, delta));
    }

    function scrollActivePane(deltaY: number, deltaX = 0) {
      const pane = document.querySelector<HTMLElement>(`[data-pane="${activePane}"]`);
      pane?.scrollBy({ top: deltaY, left: deltaX, behavior: "auto" });
    }

    function onKeyDown(event: KeyboardEvent) {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      // Keep native Tab order. Workspace tablists implement roving focus for
      // arrow/Home/End while Tab remains the user's escape hatch through the
      // complete administrative surface.
      if (isTypingTarget(event.target)) return;
      if (event.target instanceof HTMLElement && event.target.closest('[role="tablist"]')) return;

      if (event.key >= "1" && event.key <= String(workspaces.length)) {
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
      if (event.key === "ArrowDown" || event.key.toLowerCase() === "j") {
        event.preventDefault();
        if (activeWorkspace === "ledger") {
          if (activePane === "daily" || activePane === "chart") moveDay(1);
          else if (activePane === "sessions") moveSession(1);
          else scrollActivePane(72);
        } else {
          moveSession(1);
        }
        return;
      }
      if (event.key === "ArrowUp" || event.key.toLowerCase() === "k") {
        event.preventDefault();
        if (activeWorkspace === "ledger") {
          if (activePane === "daily" || activePane === "chart") moveDay(-1);
          else if (activePane === "sessions") moveSession(-1);
          else scrollActivePane(-72);
        } else {
          moveSession(-1);
        }
        return;
      }
      if (event.key === "ArrowLeft") {
        event.preventDefault();
        if (activeWorkspace === "ledger") {
          if (activePane === "daily" || activePane === "chart") moveDay(-1);
          else scrollActivePane(0, -96);
        } else moveTab(-1);
        return;
      }
      if (event.key === "ArrowRight") {
        event.preventDefault();
        if (activeWorkspace === "ledger") {
          if (activePane === "daily" || activePane === "chart") moveDay(1);
          else scrollActivePane(0, 96);
        } else moveTab(1);
        return;
      }
      if (event.key === "PageDown" && activeWorkspace === "ledger") {
        event.preventDefault();
        scrollActivePane(window.innerHeight * 0.45);
        return;
      }
      if (event.key === "PageUp" && activeWorkspace === "ledger") {
        event.preventDefault();
        scrollActivePane(window.innerHeight * -0.45);
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    activeWorkspace,
    activePane,
    activeTab,
    ledgerRows,
    query,
    selectedDayIndex,
    selectedSessionSource.length,
    shortcutsOpen
  ]);

  const commandText = `dirtydash ${activeConfig.command} --sort ${sortMode} --view ${viewMode} --graph ${chartType}`;

  return (
    <main className="tui-shell">
      <aside className="rail" aria-label="Primary workspaces">
        <a className="brand" href="/" aria-label="dirtydash home">
          <DirtydashMark />
          <span>dirtydash</span>
        </a>
        <nav className="workspace-rail">
          {workspaces.map((workspace, index) => {
            const Icon = workspace.icon;
            const active = activeWorkspace === workspace.id;
            const shortcut = String(index + 1);
            return (
              <button
                key={workspace.id}
                type="button"
                className={active ? "rail-button active" : "rail-button"}
                onClick={() => setActiveWorkspace(workspace.id)}
                aria-current={active ? "page" : undefined}
                title={`${workspace.label}. Shortcut: ${shortcut}`}
              >
                <kbd>{shortcut}</kbd>
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
              title="Search. Shortcut: /"
            />
            <kbd>/</kbd>
          </label>
          <button
            className="icon-command"
            type="button"
            onClick={() => setShortcutsOpen(true)}
            aria-label="Show shortcuts"
            title="Show shortcuts. Shortcut: ?"
          >
            <Keyboard size={17} aria-hidden="true" />
          </button>
        </header>

        <div className="workspace-header">
          <div>
            <h1>{activeConfig.label}</h1>
            <div className="mode-strip" aria-label="Dashboard state">
              <ModePill label="sort" value={sortMode} shortcut="s" />
              <ModePill label="view" value={viewMode} shortcut="v" />
              <ModePill label="chart" value={chartType} shortcut="g" />
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
            activePane={activePane}
            summary={summary}
            sources={sources}
            sessions={sortedFilteredSessions}
            daySessions={sortedDaySessions}
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
            onSelectPane={setActivePane}
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
  activePane: LedgerPaneId;
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
  onSelectPane: (pane: LedgerPaneId) => void;
  pricing: PricingRecord[];
  doctor: DoctorReport | null;
}) {
  if (props.activeWorkspace === "sources") return <SourcesWorkspace {...props} />;
  if (props.activeWorkspace === "sessions") return <SessionsWorkspace {...props} />;
  if (props.activeWorkspace === "machines") return <MachinesWorkspace activeTab={props.activeTab} />;
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
  activePane,
  sortMode,
  viewMode,
  chartType,
  dayLoading,
  onSelectDay,
  onSelectSession,
  onSelectPane
}: {
  summary: DashboardSummary;
  daySessions: SessionSummary[];
  selectedDay: string | null;
  selectedDayRow: NamedUsagePoint | null;
  selectedSession: SessionSummary | null;
  selectedSessionCursor: number;
  activePane: LedgerPaneId;
  sortMode: SortMode;
  viewMode: ViewMode;
  chartType: ChartType;
  dayLoading: boolean;
  onSelectDay: (day: string) => void;
  onSelectSession: (index: number) => void;
  onSelectPane: (pane: LedgerPaneId) => void;
}) {
  const sortedDays = useMemo(() => sortDailyRows(summary.daily, sortMode), [summary.daily, sortMode]);
  const chartRows = useMemo(() => [...sortedDays].reverse(), [sortedDays]);
  const selectedChartIndex = Math.max(
    0,
    chartRows.findIndex((row) => row.name === selectedDay)
  );

  return (
    <div className="ledger-workspace">
      <UsageGlance summary={summary} />
      <div className="ledger-grid">
        <section
          className={paneClassName("ledger-pane", activePane === "daily")}
          aria-label="Daily usage ledger"
          data-pane="daily"
          tabIndex={0}
          onFocus={() => onSelectPane("daily")}
        >
          <PaneTitle
            title="daily usage"
            meta={summary.daily.length ? `${sortMode} sort, ${summary.daily.length} days` : "no days"}
          />
          <DailyLedger
            rows={sortedDays}
            selectedDay={selectedDay}
            sortMode={sortMode}
            onSelectDay={onSelectDay}
          />
        </section>

        <section
          className={paneClassName("chart-pane", activePane === "chart")}
          aria-label="Usage chart"
          data-pane="chart"
          tabIndex={0}
          onFocus={() => onSelectPane("chart")}
        >
          <PaneTitle
            title={`${chartType} chart`}
            meta={selectedDayRow ? selectedDayRow.name : "waiting"}
          />
          <UsageChart
            rows={chartRows}
            selectedIndex={selectedChartIndex}
            chartType={chartType}
            sortMode={sortMode}
            onSelect={onSelectDay}
          />
        </section>

        <section
          className={paneClassName("sessions-pane", activePane === "sessions")}
          aria-label="Selected day sessions"
          data-pane="sessions"
          tabIndex={0}
          onFocus={() => onSelectPane("sessions")}
        >
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
          active={activePane === "inspector"}
          paneId="inspector"
          onFocusPane={() => onSelectPane("inspector")}
        />
      </div>
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
          <PaneTitle title="trace details" meta={selectedSession ? sessionLabel(selectedSession) : "none"} />
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
            ["cache", percent(summary.cache.cache_read_share), "read share"],
            ["saved", money(summary.cache.estimated_savings_usd), "cache discount"]
          ]}
        />
      </section>
    </div>
  );
}

function UsageGlance({ summary }: { summary: DashboardSummary }) {
  const topModel = topUsageShare(summary.by_model, summary.totals.total_tokens);
  const topEffort = topUsageShare(summary.by_reasoning_effort, summary.totals.total_tokens);
  return (
    <section className="usage-glance" aria-label="Ledger usage summary">
      <div className="glance-primary">
        <span>cumulative</span>
        <strong>{money(summary.totals.estimated_cost_usd)}</strong>
        <small>{compact(summary.totals.total_tokens)} tokens</small>
      </div>
      <div>
        <span>model</span>
        <strong>{topModel?.name ?? "unknown"}</strong>
        <small>{topModel ? percent(topModel.share) : "0%"}</small>
      </div>
      <div>
        <span>reasoning</span>
        <strong>{topEffort?.name ?? "unknown"}</strong>
        <small>{topEffort ? percent(topEffort.share) : "0%"}</small>
      </div>
      <div>
        <span>cache</span>
        <strong>{percent(summary.cache.cache_read_share)}</strong>
        <small>{compact(summary.cache.cache_read_tokens)} read / {money(summary.cache.estimated_savings_usd)} saved</small>
      </div>
    </section>
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
  const yTicks = [1, 2 / 3, 1 / 3, 0];
  const points = rows.map((row, index) => {
    const x = pad.left + (rows.length <= 1 ? chartWidth / 2 : (index / (rows.length - 1)) * chartWidth);
    const y = pad.top + chartHeight - (metricValue(row, sortMode) / max) * chartHeight;
    return { row, x, y };
  });
  const tooltipPoint = hovered === null ? null : points[hovered] ?? null;
  const linePath = points
    .map((point, index) => `${index === 0 ? "M" : "L"} ${point.x.toFixed(1)} ${point.y.toFixed(1)}`)
    .join(" ");

  return (
    <div className="chart-wrap">
      <svg className="usage-chart" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={`${chartType} usage chart`}>
        <g className="chart-grid">
          {yTicks.map((tick) => {
            const y = pad.top + chartHeight - chartHeight * tick;
            return (
              <React.Fragment key={tick}>
                <text x={pad.left - 8} y={y + 4} textAnchor="end">
                  {axisValue(max * tick, sortMode)}
                </text>
                <line x1={pad.left} x2={width - pad.right} y1={y} y2={y} />
              </React.Fragment>
            );
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
          <text x={pad.left} y={height - 8}>{rows[0]?.name ?? "no data"}</text>
          <text x={(pad.left + width - pad.right) / 2} y={height - 8} textAnchor="middle">{sortMode}</text>
          <text x={width - pad.right} y={height - 8} textAnchor="end">{rows[rows.length - 1]?.name ?? "no data"}</text>
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
            title={`Session ${session.session_id}`}
          >
            <span aria-hidden="true">{active ? ">" : ""}</span>
            <code>{sessionLabel(session)}</code>
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
  sortMode,
  active = false,
  paneId,
  onFocusPane
}: {
  day?: NamedUsagePoint | null;
  session: SessionSummary | null;
  viewMode: ViewMode;
  sortMode: SortMode;
  active?: boolean;
  paneId?: LedgerPaneId;
  onFocusPane?: () => void;
}) {
  return (
    <aside
      className={paneClassName("inspector-pane", active)}
      aria-label="Inspector"
      data-pane={paneId}
      tabIndex={paneId ? 0 : undefined}
      onFocus={onFocusPane}
    >
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
            <DetailRow label="session" value={sessionLabel(session)} />
            <DetailRow label="project" value={session.project_path} />
            <DetailRow label="model" value={session.model} />
            <DetailRow label="source" value={`${session.machine}/${session.source}`} />
            <DetailRow label="confidence" value={percent(session.confidence)} />
          </dl>
          <div className="provenance-stack">
            <code>id={session.session_id}</code>
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
              <td title={session.session_id}>{sessionLabel(session)}</td>
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
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const activeIndex = Math.max(0, tabs.indexOf(activeTab));

  useEffect(() => {
    tabRefs.current[activeIndex]?.focus({ preventScroll: true });
  }, [activeIndex]);

  function moveTab(index: number) {
    const next = (index + tabs.length) % tabs.length;
    const tab = tabs[next];
    if (tab) onSelect(tab);
  }

  function onKeyDown(event: React.KeyboardEvent<HTMLButtonElement>) {
    if (event.key === "ArrowRight" || event.key === "ArrowDown") {
      event.preventDefault();
      moveTab(activeIndex + 1);
    } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
      event.preventDefault();
      moveTab(activeIndex - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      moveTab(0);
    } else if (event.key === "End") {
      event.preventDefault();
      moveTab(tabs.length - 1);
    }
  }

  return (
    <div className="tabs-wrap">
      <div className="tabs" role="tablist" aria-label="Workspace views" title="Arrow keys move views; Tab leaves the tablist">
        {tabs.map((tab, index) => (
          <button
            key={tab}
            ref={(element) => { tabRefs.current[index] = element; }}
            type="button"
            role="tab"
            aria-selected={activeTab === tab}
            tabIndex={activeTab === tab ? 0 : -1}
            className={activeTab === tab ? "active" : ""}
            onKeyDown={onKeyDown}
            onClick={() => onSelect(tab)}
            title={`${tab}. Arrow keys move views`}
          >
            {tab}
          </button>
        ))}
      </div>
      <span className="tab-hint"><kbd>←</kbd><kbd>→</kbd> move · <kbd>Tab</kbd> continue</span>
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

function paneClassName(className: string, active: boolean) {
  return active ? `pane ${className} active-pane` : `pane ${className}`;
}

function ModePill({ label, value, shortcut }: { label: string; value: string; shortcut?: string }) {
  return (
    <span className="mode-pill" title={shortcut ? `${label}. Shortcut: ${shortcut}` : undefined}>
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
  const closeRef = useRef<HTMLButtonElement>(null);
  const previousFocus = useRef<HTMLElement | null>(null);
  const rows = [
    ["1-5", "switch workspace"],
    ["/", "focus search"],
    ["Tab", "native focus order"],
    ["↑/↓", "move row or scroll pane"],
    ["←/→", "move day or scroll pane"],
    ["PgUp/PgDn", "scroll active pane"],
    ["j/k", "move row or scroll pane"],
    ["s", "cycle sort"],
    ["v", "cycle view"],
    ["g", "cycle chart"],
    ["?", "shortcuts"],
    ["Esc", "clear or close"]
  ];

  useEffect(() => {
    previousFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    closeRef.current?.focus();
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key === "Tab") {
        event.preventDefault();
        closeRef.current?.focus();
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      previousFocus.current?.focus();
    };
  }, [onClose]);

  return (
    <div className="overlay" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts">
      <div className="shortcut-pane" tabIndex={-1}>
        <PaneTitle title="shortcuts" meta="keyboard-first" />
        <dl>
          {rows.map(([key, value]) => (
            <div key={key}>
              <dt><kbd>{key}</kbd></dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
        <button ref={closeRef} type="button" onClick={onClose}>close</button>
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
    if (mode === "day") return sessionTime(b) - sessionTime(a);
    return b.estimated_cost_usd - a.estimated_cost_usd;
  });
}

function sortDailyRows(rows: NamedUsagePoint[], mode: SortMode) {
  return [...rows].sort((a, b) => {
    if (mode === "tokens") return b.total_tokens - a.total_tokens;
    if (mode === "cost") return b.estimated_cost_usd - a.estimated_cost_usd;
    return b.name.localeCompare(a.name);
  });
}

function metricValue(row: NamedUsagePoint, mode: SortMode) {
  if (mode === "cost") return row.estimated_cost_usd;
  return row.total_tokens;
}

function axisValue(value: number, mode: SortMode) {
  if (mode === "cost") return money(value);
  return compact(Math.round(value));
}

function chartLabel(row: NamedUsagePoint, mode: SortMode) {
  return `${row.name}: ${money(row.estimated_cost_usd)}, ${compact(row.total_tokens)} tokens, ${compact(row.cache_read_tokens)} cache, ${compact(row.priority_tokens)} fast, sort ${mode}`;
}

function cacheShare(row: NamedUsagePoint) {
  const input = row.prompt_tokens + row.cache_read_tokens + row.cache_write_tokens;
  return input === 0 ? 0 : row.cache_read_tokens / input;
}

function cycleValue<T>(values: T[], current: T): T {
  const index = values.indexOf(current);
  return values[(index + 1) % values.length];
}

function cycleValueBy<T>(values: T[], current: T, delta: number): T {
  const index = Math.max(0, values.indexOf(current));
  return values[(index + delta + values.length) % values.length];
}

function topUsageShare(rows: NamedUsagePoint[], totalTokens: number) {
  const row = [...rows].sort((a, b) => b.total_tokens - a.total_tokens)[0];
  if (!row || totalTokens <= 0) return null;
  return {
    name: row.name,
    share: row.total_tokens / totalTokens
  };
}

function sessionTime(session: SessionSummary) {
  const timestamp = session.last_seen ?? session.first_seen;
  if (!timestamp) return 0;
  const value = new Date(timestamp).getTime();
  return Number.isFinite(value) ? value : 0;
}

function sessionLabel(session: SessionSummary) {
  const timestamp = session.first_seen ?? session.last_seen;
  return timestamp ? sessionDate(timestamp) : "unknown time";
}

function isTypingTarget(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || target.isContentEditable;
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

function sessionDate(value: string) {
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
