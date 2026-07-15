import React, { FormEvent, useEffect, useRef, useState } from "react";
import {
  Archive,
  ArrowDownToLine,
  ArrowUpCircle,
  CheckCircle2,
  CircleAlert,
  CircleDot,
  CircleOff,
  Clock3,
  CloudOff,
  FileKey2,
  FileSearch,
  KeyRound,
  LoaderCircle,
  Monitor,
  RefreshCw,
  RotateCw,
  Server,
  ShieldCheck,
  Trash2,
  Undo2,
  UploadCloud,
  Wifi,
  Wrench,
  XCircle
} from "lucide-react";

export type MachineHealth =
  | "online"
  | "syncing"
  | "stale"
  | "offline"
  | "update-available"
  | "action-required"
  | "archived";

export type ProtocolCompatibility = "current" | "previous" | "unsupported" | "unknown";

export interface MachineDiagnostics {
  watcher_degraded: boolean;
  credential_rotation_pending: boolean;
  terminal_outbox: number;
  pending_outbox: number;
  last_reconciliation_at?: string | null;
  last_error?: string | null;
}

export interface MachineRecord {
  machine_id: string;
  display_name: string;
  lifecycle: "active" | "archived";
  status: MachineHealth;
  status_reason: string;
  enrolled_at: string;
  archived_at?: string | null;
  last_seen_at?: string | null;
  last_sync_at?: string | null;
  collector_version?: string | null;
  desired_version?: string | null;
  collector_protocol_version?: number | null;
  protocol_compatibility: ProtocolCompatibility;
  diagnostics_status?: string | null;
  diagnostics_at?: string | null;
  diagnostics?: MachineDiagnostics | null;
  credentials_active: number;
  credentials_total: number;
  pending_action?: string | null;
  usage_event_count: number;
  state_revision: number;
}

export interface EnrollmentDraft {
  id: string;
  machine_id?: string | null;
  display_name?: string | null;
  state: string;
  blocker: string;
  host_fingerprint?: string | null;
  plan_hash?: string | null;
  reviewed_plan_hash?: string | null;
  execution_substate: string;
  receipt?: { status: string; release: string; hub_health_verified: boolean; collector_health_verified: boolean } | null;
  last_error?: string | null;
  cleanup_complete: boolean;
  updated_at: string;
  plan?: { plan_hash: string; release: string; steps?: Array<{ id: string; description: string }> } | null;
}

export interface FleetUpdateEvidence {
  version: string;
  artifact_sha256: string;
  publisher_key_id: string;
  publisher_fingerprint: string;
  manifest_sha256: string;
  signature_verified: boolean;
}

export interface FleetUpdateNode {
  update_id: string;
  machine_id: string;
  status: string;
  previous_version?: string | null;
  snapshot_at?: string | null;
  update_started_at?: string | null;
  restarted_at?: string | null;
  health_checked_at?: string | null;
  rolled_back_at?: string | null;
  failure_reason?: string | null;
  collector_protocol_version?: number | null;
  evidence?: FleetUpdateEvidence | null;
  attempts: number;
  state_revision: number;
}

export interface FleetUpdateRun {
  update_id: string;
  version: string;
  artifact_sha256: string;
  publisher_key_id: string;
  publisher_fingerprint: string;
  manifest_sha256: string;
  status: string;
  created_at: string;
  started_at?: string | null;
  hub_snapshot_at?: string | null;
  hub_updated_at?: string | null;
  hub_health_at?: string | null;
  completed_at?: string | null;
  failure_reason?: string | null;
  attempts: number;
  state_revision: number;
  nodes: FleetUpdateNode[];
}

type ApiError = Error & { status?: number; code?: string };

type Action = "refresh" | "repair" | "diagnostics" | "rotate";

const enrollmentSteps = [
  ["target-draft", "target"],
  ["host-trust-auth", "host trust + auth"],
  ["probe-and-plan", "probe + plan"],
  ["immutable-plan-review", "review"],
  ["execute-verify-receipt", "execute + receipt"]
] as const;

const healthIcon: Record<MachineHealth, React.ComponentType<{ size?: number; "aria-hidden"?: boolean }>> = {
  online: Wifi,
  syncing: RefreshCw,
  stale: Clock3,
  offline: CloudOff,
  "update-available": ArrowUpCircle,
  "action-required": CircleAlert,
  archived: Archive
};

const healthLabel: Record<MachineHealth, string> = {
  online: "online",
  syncing: "syncing",
  stale: "stale",
  offline: "offline",
  "update-available": "update available",
  "action-required": "action required",
  archived: "archived"
};

const protocolLabel: Record<ProtocolCompatibility, string> = {
  current: "protocol current",
  previous: "protocol previous",
  unsupported: "protocol unsupported",
  unknown: "protocol unknown"
};

export function machineHealthLabel(status: MachineHealth) {
  return healthLabel[status];
}

export function protocolCompatibilityLabel(status: ProtocolCompatibility) {
  return protocolLabel[status];
}

export function isMachineAdminWidth(width: number) {
  return width >= 760;
}

function StatusBadge({ status }: { status: MachineHealth }) {
  const Icon = healthIcon[status] ?? CircleDot;
  return (
    <span className={`machine-status ${status}`} data-status={status}>
      <Icon size={15} aria-hidden="true" />
      <span>{healthLabel[status]}</span>
    </span>
  );
}

function ProtocolBadge({ status }: { status: ProtocolCompatibility }) {
  const Icon = status === "unsupported" ? XCircle : status === "previous" ? Clock3 : CheckCircle2;
  return (
    <span className={`protocol-status ${status}`}>
      <Icon size={14} aria-hidden="true" />
      <span>{protocolLabel[status]}</span>
    </span>
  );
}

function UpdateStatusBadge({ status }: { status: string }) {
  const complete = status === "completed";
  const failed = status === "failed" || status === "completed-with-failures";
  const Icon = complete ? CheckCircle2 : failed ? CircleAlert : status === "hub-updating" ? Server : UploadCloud;
  return <span className={`update-run-status ${failed ? "failed" : complete ? "complete" : "pending"}`}><Icon size={14} aria-hidden="true" /> {status}</span>;
}

function Busy({ label }: { label: string }) {
  return (
    <span className="busy-label" role="status" aria-live="polite">
      <LoaderCircle size={15} aria-hidden="true" /> {label}
    </span>
  );
}

function InlineError({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return (
    <div className="machine-error" role="alert">
      <CircleAlert size={17} aria-hidden="true" />
      <span>{message}</span>
      {onRetry ? (
        <button type="button" className="button subtle" onClick={onRetry}>
          retry
        </button>
      ) : null}
    </div>
  );
}

function InlineEmpty({ title, detail, action }: { title: string; detail: string; action?: React.ReactNode }) {
  return (
    <div className="machine-empty">
      <CircleOff size={22} aria-hidden="true" />
      <strong>{title}</strong>
      <p>{detail}</p>
      {action}
    </div>
  );
}

function apiError(response: Response, body: unknown): ApiError {
  const error = new Error(
    typeof body === "object" && body && "message" in body && typeof body.message === "string"
      ? body.message
      : `Hub request failed (${response.status})`
  ) as ApiError;
  error.status = response.status;
  if (typeof body === "object" && body && "code" in body && typeof body.code === "string") error.code = body.code;
  return error;
}

async function api<T>(url: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(url, {
    credentials: "include",
    ...init,
    headers: {
      Accept: "application/json",
      ...(init.body ? { "Content-Type": "application/json" } : {}),
      ...(init.headers ?? {})
    }
  });
  const text = await response.text();
  let body: unknown = null;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = text;
    }
  }
  if (!response.ok) throw apiError(response, body);
  return body as T;
}

function postJson<T>(url: string, body: unknown, csrf: string) {
  return api<T>(url, {
    method: "POST",
    headers: { "x-csrf-token": csrf },
    body: JSON.stringify(body)
  });
}

function useDesktopAdmin() {
  const [desktop, setDesktop] = useState(() =>
    typeof window === "undefined" ? true : isMachineAdminWidth(window.innerWidth)
  );
  useEffect(() => {
    const update = () => setDesktop(isMachineAdminWidth(window.innerWidth));
    update();
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, []);
  return desktop;
}

export function MachinesWorkspace({ activeTab }: { activeTab: string }) {
  const desktopAdmin = useDesktopAdmin();
  const [machines, setMachines] = useState<MachineRecord[]>([]);
  const [updates, setUpdates] = useState<FleetUpdateRun[]>([]);
  const [enrollments, setEnrollments] = useState<EnrollmentDraft[]>([]);
  const [selectedMachineId, setSelectedMachineId] = useState<string | null>(null);
  const [csrf, setCsrf] = useState("");
  const [authenticated, setAuthenticated] = useState(true);
  const [loading, setLoading] = useState(true);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try {
      const csrfResponse = await api<{ csrf_token: string }>("/api/v1/admin/session/csrf");
      setCsrf(csrfResponse.csrf_token);
      const [machineRows, updateRows, enrollmentRows] = await Promise.all([
        api<MachineRecord[]>("/api/v1/admin/machines"),
        api<FleetUpdateRun[]>("/api/v1/admin/updates"),
        api<EnrollmentDraft[]>("/api/v1/admin/enrollment")
      ]);
      setMachines(machineRows);
      setUpdates(updateRows);
      setEnrollments(enrollmentRows);
      setAuthenticated(true);
      setSelectedMachineId((current) =>
        current && machineRows.some((machine) => machine.machine_id === current)
          ? current
          : machineRows[0]?.machine_id ?? null
      );
    } catch (loadError) {
      const typed = loadError as ApiError;
      setAuthenticated(typed.status !== 401 && typed.status !== 403);
      setError(typed.message);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const selectedMachine = machines.find((machine) => machine.machine_id === selectedMachineId) ?? null;

  async function runAction(machine: MachineRecord, action: Action) {
    if (!desktopAdmin) return;
    setWorking(true);
    setError(null);
    try {
      await postJson<unknown>(
        `/api/v1/admin/machines/${encodeURIComponent(machine.machine_id)}/${action}`,
        { expected_state_revision: machine.state_revision },
        csrf
      );
      await refresh();
    } catch (actionError) {
      setError((actionError as Error).message);
    } finally {
      setWorking(false);
    }
  }

  async function archiveMachine(machine: MachineRecord, remove = false) {
    if (!desktopAdmin) return;
    setWorking(true);
    setError(null);
    try {
      await postJson<MachineRecord>(
        `/api/v1/admin/machines/${encodeURIComponent(machine.machine_id)}/${remove ? "remove" : "archive"}`,
        { expected_state_revision: machine.state_revision, display_name: machine.display_name },
        csrf
      );
      await refresh();
    } catch (actionError) {
      setError((actionError as Error).message);
    } finally {
      setWorking(false);
    }
  }

  async function deleteMachine(machine: MachineRecord) {
    if (!desktopAdmin) return;
    setWorking(true);
    setError(null);
    try {
      await postJson<unknown>(
        `/api/v1/admin/machines/${encodeURIComponent(machine.machine_id)}/delete`,
        {
          expected_state_revision: machine.state_revision,
          display_name: machine.display_name,
          confirmation: `DELETE ${machine.display_name}`
        },
        csrf
      );
      setSelectedMachineId(null);
      await refresh();
    } catch (actionError) {
      setError((actionError as Error).message);
    } finally {
      setWorking(false);
    }
  }

  if (!authenticated) {
    return <HubLogin onAuthenticated={(token) => { setCsrf(token); void refresh(); }} />;
  }

  return (
    <div className="machines-workspace">
      <header className="machines-header">
        <div>
          <div className="section-kicker"><Server size={14} aria-hidden="true" /> hosted Hub / Machines</div>
          <h2>Machine control surface</h2>
          <p className="machines-subtitle">Metadata-only observations. Lifecycle actions stay on the Hub and are never inferred by the browser.</p>
        </div>
        <div className="machines-header-actions">
          {loading ? <Busy label="loading fleet" /> : null}
          <button type="button" className="button" onClick={() => void refresh()} disabled={loading || working}>
            <RefreshCw size={15} aria-hidden="true" /> refresh fleet
          </button>
        </div>
      </header>
      {!desktopAdmin ? (
        <div className="read-only-notice" role="note">
          <Monitor size={17} aria-hidden="true" /> <strong>read-only at this width</strong>
          <span>Use a tablet or desktop to enroll, repair, rotate, archive, delete, or update Machines.</span>
        </div>
      ) : null}
      {error ? <InlineError message={error} onRetry={() => void refresh()} /> : null}
      {loading ? <MachineSkeleton /> : null}
      {!loading && !error && activeTab === "fleet" ? (
        <FleetTab
          machines={machines}
          selectedMachine={selectedMachine}
          desktopAdmin={desktopAdmin}
          working={working}
          onSelect={setSelectedMachineId}
          onAction={(machine, action) => void runAction(machine, action)}
          onArchive={(machine) => void archiveMachine(machine)}
          onRemove={(machine) => void archiveMachine(machine, true)}
          onDelete={(machine) => void deleteMachine(machine)}
        />
      ) : null}
      {!loading && !error && activeTab === "enroll" ? (
        <EnrollmentTab
          csrf={csrf}
          desktopAdmin={desktopAdmin}
          drafts={enrollments}
          onChange={() => void refresh()}
        />
      ) : null}
      {!loading && !error && activeTab === "updates" ? (
        <UpdatesTab csrf={csrf} desktopAdmin={desktopAdmin} updates={updates} machines={machines} onChange={() => void refresh()} />
      ) : null}
      {!loading && !error && activeTab === "history" ? <UpdateHistory updates={updates} /> : null}
    </div>
  );
}

function MachineSkeleton() {
  return (
    <div className="machine-skeleton" aria-label="Loading Machines">
      <div />
      <div />
      <div />
      <div />
    </div>
  );
}

function HubLogin({ onAuthenticated }: { onAuthenticated: (csrf: string) => void }) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);
  async function submit(event: FormEvent) {
    event.preventDefault();
    setWorking(true);
    setError(null);
    try {
      const response = await api<{ csrf_token: string }>("/api/v1/admin/session/login", {
        method: "POST",
        body: JSON.stringify({ username, password })
      });
      setPassword("");
      onAuthenticated(response.csrf_token);
    } catch (loginError) {
      setError((loginError as Error).message);
    } finally {
      setWorking(false);
    }
  }
  return (
    <section className="machines-auth pane" aria-labelledby="hub-login-title">
      <div className="section-kicker"><LockIcon /> authenticated Hub</div>
      <h2 id="hub-login-title">Sign in to operate Machines</h2>
      <p>Machine inventory, enrollment, credentials, and updates are owner-session resources.</p>
      {error ? <InlineError message={error} /> : null}
      <form className="inline-form" onSubmit={submit}>
        <label>username<input value={username} onChange={(event) => setUsername(event.target.value)} autoComplete="username" required /></label>
        <label>password<input type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="current-password" required /></label>
        <button className="button primary" type="submit" disabled={working}>{working ? <Busy label="signing in" /> : <><ShieldCheck size={15} aria-hidden="true" /> sign in</>}</button>
      </form>
    </section>
  );
}

function LockIcon() {
  return <KeyRound size={14} aria-hidden="true" />;
}

function FleetTab({
  machines,
  selectedMachine,
  desktopAdmin,
  working,
  onSelect,
  onAction,
  onArchive,
  onRemove,
  onDelete
}: {
  machines: MachineRecord[];
  selectedMachine: MachineRecord | null;
  desktopAdmin: boolean;
  working: boolean;
  onSelect: (id: string) => void;
  onAction: (machine: MachineRecord, action: Action) => void;
  onArchive: (machine: MachineRecord) => void;
  onRemove: (machine: MachineRecord) => void;
  onDelete: (machine: MachineRecord) => void;
}) {
  return (
    <div className="machine-split">
      <section className="machine-table-pane pane" aria-labelledby="fleet-table-title">
        <div className="pane-title"><h2 id="fleet-table-title">fleet</h2><span>{machines.length} Machines / state from Hub</span></div>
        {machines.length === 0 ? (
          <InlineEmpty title="No Machines enrolled" detail="Start in Enroll to create a resumable Hub-side SSH enrollment draft." />
        ) : (
          <div className="machine-table-scroll">
            <table className="machine-table">
              <thead><tr><th>Machine</th><th>state</th><th>protocol</th><th>collector</th><th>last seen</th><th>events</th></tr></thead>
              <tbody>
                {machines.map((machine) => (
                  <tr key={machine.machine_id} className={selectedMachine?.machine_id === machine.machine_id ? "selected" : undefined}>
                    <td>
                      <button type="button" className="machine-row-button" onClick={() => onSelect(machine.machine_id)}>
                        <strong>{machine.display_name}</strong><code>{machine.machine_id}</code>
                      </button>
                    </td>
                    <td><StatusBadge status={machine.status} /><small>{machine.status_reason}</small></td>
                    <td><ProtocolBadge status={machine.protocol_compatibility} /></td>
                    <td><code>{machine.collector_version ?? "not reported"}</code>{machine.desired_version ? <small>desired {machine.desired_version}</small> : null}</td>
                    <td>{formatAge(machine.last_seen_at)}</td>
                    <td>{compact(machine.usage_event_count)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
      <MachineDetail
        machine={selectedMachine}
        desktopAdmin={desktopAdmin}
        working={working}
        onAction={onAction}
        onArchive={onArchive}
        onRemove={onRemove}
        onDelete={onDelete}
      />
    </div>
  );
}

function MachineDetail({
  machine,
  desktopAdmin,
  working,
  onAction,
  onArchive,
  onRemove,
  onDelete
}: {
  machine: MachineRecord | null;
  desktopAdmin: boolean;
  working: boolean;
  onAction: (machine: MachineRecord, action: Action) => void;
  onArchive: (machine: MachineRecord) => void;
  onRemove: (machine: MachineRecord) => void;
  onDelete: (machine: MachineRecord) => void;
}) {
  const [archiveOpen, setArchiveOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deleteConfirmation, setDeleteConfirmation] = useState("");
  const archiveButtonRef = useRef<HTMLButtonElement>(null);
  const deleteButtonRef = useRef<HTMLButtonElement>(null);
  const archiveConfirmRef = useRef<HTMLButtonElement>(null);
  const deleteInputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    setArchiveOpen(false);
    setDeleteOpen(false);
    setDeleteConfirmation("");
  }, [machine?.machine_id]);
  useEffect(() => { if (archiveOpen) archiveConfirmRef.current?.focus(); }, [archiveOpen]);
  useEffect(() => { if (deleteOpen) deleteInputRef.current?.focus(); }, [deleteOpen]);
  useEffect(() => {
    if (!archiveOpen && !deleteOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      if (archiveOpen) {
        setArchiveOpen(false);
        archiveButtonRef.current?.focus();
      } else {
        setDeleteOpen(false);
        setDeleteConfirmation("");
        deleteButtonRef.current?.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [archiveOpen, deleteOpen]);
  if (!machine) {
    return <aside className="machine-detail pane"><InlineEmpty title="Select a Machine" detail="The detail pane keeps lifecycle evidence and destructive actions separate from the fleet table." /></aside>;
  }
  return (
    <aside className="machine-detail pane" aria-labelledby="machine-detail-title">
      <div className="pane-title"><h2 id="machine-detail-title">Machine detail</h2><span>revision {machine.state_revision}</span></div>
      <div className="detail-identity"><Server size={19} aria-hidden="true" /><div><strong>{machine.display_name}</strong><code>{machine.machine_id}</code></div></div>
      <StatusBadge status={machine.status} />
      <p className="detail-reason">{machine.status_reason}</p>
      <dl className="machine-facts">
        <div><dt>protocol</dt><dd><ProtocolBadge status={machine.protocol_compatibility} />{machine.collector_protocol_version ?? "not reported"}</dd></div>
        <div><dt>collector</dt><dd><code>{machine.collector_version ?? "not reported"}</code></dd></div>
        <div><dt>desired</dt><dd><code>{machine.desired_version ?? "—"}</code></dd></div>
        <div><dt>last seen</dt><dd>{formatAbsolute(machine.last_seen_at)}</dd></div>
        <div><dt>last sync</dt><dd>{formatAbsolute(machine.last_sync_at)}</dd></div>
        <div><dt>credentials</dt><dd>{machine.credentials_active} active / {machine.credentials_total} retained</dd></div>
        <div><dt>history</dt><dd>{compact(machine.usage_event_count)} metadata events</dd></div>
      </dl>
      {machine.diagnostics ? <DiagnosticsSummary diagnostics={machine.diagnostics} /> : <p className="muted-line">No Collector diagnostics receipt yet.</p>}
      {machine.pending_action ? <p className="pending-line"><RefreshCw size={14} aria-hidden="true" /> command {machine.pending_action} awaiting acknowledgement</p> : null}
      <div className="machine-actions desktop-admin" aria-label="Machine actions">
        <button type="button" className="button" disabled={!desktopAdmin || working || machine.lifecycle === "archived"} onClick={() => onAction(machine, "refresh")}><RefreshCw size={15} aria-hidden="true" /> refresh</button>
        <button type="button" className="button" disabled={!desktopAdmin || working || machine.lifecycle === "archived"} onClick={() => onAction(machine, "repair")}><Wrench size={15} aria-hidden="true" /> repair</button>
        <button type="button" className="button" disabled={!desktopAdmin || working || machine.lifecycle === "archived"} onClick={() => onAction(machine, "diagnostics")}><FileSearch size={15} aria-hidden="true" /> diagnostics</button>
        <button type="button" className="button" disabled={!desktopAdmin || working || machine.lifecycle === "archived"} onClick={() => onAction(machine, "rotate")}><RotateCw size={15} aria-hidden="true" /> rotate credentials</button>
      </div>
      <div className="lifecycle-disclosures desktop-admin">
        <div className="disclosure-block">
          <button ref={archiveButtonRef} type="button" className="disclosure-trigger" disabled={!desktopAdmin || working || machine.lifecycle === "archived"} aria-expanded={archiveOpen} aria-controls="archive-confirmation" onClick={() => setArchiveOpen((open) => !open)}><Archive size={15} aria-hidden="true" /> archive Machine</button>
          {archiveOpen ? <div id="archive-confirmation" className="inline-confirm" role="dialog" aria-modal="true" aria-labelledby="archive-title"><strong id="archive-title">Archive, do not delete</strong><p>Revokes active Collector credentials and retains credentials plus history. This is reversible only by a new explicit enrollment.</p><div><button ref={archiveConfirmRef} type="button" className="button danger" disabled={working} onClick={() => { onArchive(machine); setArchiveOpen(false); archiveButtonRef.current?.focus(); }}>archive and revoke</button><button type="button" className="button subtle" onClick={() => { setArchiveOpen(false); archiveButtonRef.current?.focus(); }}>cancel</button></div></div> : null}
        </div>
        <div className="disclosure-block">
          <button ref={deleteButtonRef} type="button" className="disclosure-trigger destructive" disabled={!desktopAdmin || working || machine.lifecycle !== "archived"} aria-expanded={deleteOpen} aria-controls="delete-confirmation" onClick={() => setDeleteOpen((open) => !open)}><Trash2 size={15} aria-hidden="true" /> permanently delete</button>
          {deleteOpen ? <div id="delete-confirmation" className="inline-confirm danger-box" role="dialog" aria-modal="true" aria-labelledby="delete-title"><strong id="delete-title">Permanent deletion</strong><p>Deletes this archived Machine, credentials, commands, update records, and usage history in one transaction. Type confirmation is required by the Hub.</p><code>DELETE {machine.display_name}</code><label>type confirmation<input ref={deleteInputRef} value={deleteConfirmation} onChange={(event) => setDeleteConfirmation(event.target.value)} aria-describedby="delete-title" /></label><div><button ref={deleteConfirmRef} type="button" className="button danger" disabled={working || deleteConfirmation !== `DELETE ${machine.display_name}`} onClick={() => { onDelete(machine); setDeleteOpen(false); setDeleteConfirmation(""); deleteButtonRef.current?.focus(); }}>delete permanently</button><button type="button" className="button subtle" onClick={() => { setDeleteOpen(false); setDeleteConfirmation(""); deleteButtonRef.current?.focus(); }}>cancel</button></div></div> : null}
        </div>
        <button type="button" className="disclosure-trigger" disabled={!desktopAdmin || working || machine.lifecycle === "archived"} onClick={() => onRemove(machine)}><Undo2 size={15} aria-hidden="true" /> remove / revoke (retain history)</button>
      </div>
    </aside>
  );
}

function DiagnosticsSummary({ diagnostics }: { diagnostics: MachineDiagnostics }) {
  const actionRequired = diagnostics.watcher_degraded || diagnostics.credential_rotation_pending || diagnostics.terminal_outbox > 0;
  return <div className={`diagnostics-summary ${actionRequired ? "attention" : "healthy"}`}><span>{actionRequired ? <CircleAlert size={14} aria-hidden="true" /> : <CheckCircle2 size={14} aria-hidden="true" />} {actionRequired ? "diagnostics require attention" : "diagnostics healthy"}</span><small>{diagnostics.pending_outbox} pending / {diagnostics.terminal_outbox} terminal outbox</small></div>;
}

function EnrollmentTab({ csrf, desktopAdmin, drafts, onChange }: { csrf: string; desktopAdmin: boolean; drafts: EnrollmentDraft[]; onChange: () => void }) {
  const [selectedId, setSelectedId] = useState<string | null>(drafts[0]?.id ?? null);
  const [form, setForm] = useState({ id: "machine-draft", machine_id: "machine-", display_name: "", alias: "", user: "", host: "", port: "22", auth: "password" });
  const [secrets, setSecrets] = useState({ password: "", key_passphrase: "", sudo_password: "" });
  const [confirmFingerprint, setConfirmFingerprint] = useState("");
  const [artifact, setArtifact] = useState({ manifest: "", bytes: "", seed: "" });
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const selected = drafts.find((draft) => draft.id === selectedId) ?? null;
  useEffect(() => { if (!selectedId && drafts[0]) setSelectedId(drafts[0].id); }, [drafts, selectedId]);
  async function submitCreate(event: FormEvent) {
    event.preventDefault();
    if (!desktopAdmin) return;
    setWorking(true); setError(null);
    try {
      const connection = form.alias ? { kind: "alias", alias: form.alias } : { kind: "manual", user: form.user, host: form.host, port: Number(form.port) };
      const auth = form.auth === "password" ? "password" : { "key-path": { path: form.auth } };
      const draft = await postJson<EnrollmentDraft>("/api/v1/admin/enrollment", { id: form.id, machine_id: form.machine_id, display_name: form.display_name, connection, auth }, csrf);
      setSelectedId(draft.id); onChange();
    } catch (createError) { setError((createError as Error).message); } finally { setWorking(false); }
  }
  async function step(path: string, body: unknown) {
    if (!selected || !desktopAdmin) return;
    setWorking(true); setError(null);
    try { await postJson<unknown>(`/api/v1/admin/enrollment/${encodeURIComponent(selected.id)}/${path}`, body, csrf); onChange(); } catch (stepError) { setError((stepError as Error).message); } finally { setWorking(false); }
  }
  const secretBody = { password: secrets.password || undefined, key_passphrase: secrets.key_passphrase || undefined, sudo_password: secrets.sudo_password || undefined };
  return (
    <div className="enrollment-layout">
      <section className="enrollment-create pane" aria-labelledby="enroll-create-title">
        <div className="pane-title"><h2 id="enroll-create-title">new enrollment</h2><span>Hub-side SSH / resumable</span></div>
        <form className="enrollment-form" onSubmit={submitCreate}>
          <label>draft id<input value={form.id} onChange={(event) => setForm({ ...form, id: event.target.value })} required /></label>
          <label>Machine id<input value={form.machine_id} onChange={(event) => setForm({ ...form, machine_id: event.target.value })} required /></label>
          <label>display name<input value={form.display_name} onChange={(event) => setForm({ ...form, display_name: event.target.value })} required /></label>
          <fieldset><legend>connection</legend><label>SSH alias<input value={form.alias} onChange={(event) => setForm({ ...form, alias: event.target.value })} placeholder="workstation" /></label><span className="or-line">or manual target</span><div className="field-row"><label>user<input value={form.user} onChange={(event) => setForm({ ...form, user: event.target.value })} /></label><label>host<input value={form.host} onChange={(event) => setForm({ ...form, host: event.target.value })} /></label><label>port<input type="number" min="1" max="65535" value={form.port} onChange={(event) => setForm({ ...form, port: event.target.value })} /></label></div></fieldset>
          <label>authentication<select value={form.auth} onChange={(event) => setForm({ ...form, auth: event.target.value })}><option value="password">password + sudo as needed</option><option value="~/.ssh/id_ed25519">key path: ~/.ssh/id_ed25519</option></select></label>
          <label>SSH password<input type="password" value={secrets.password} onChange={(event) => setSecrets({ ...secrets, password: event.target.value })} autoComplete="off" /></label>
          <label>sudo password<input type="password" value={secrets.sudo_password} onChange={(event) => setSecrets({ ...secrets, sudo_password: event.target.value })} autoComplete="off" /></label>
          {error ? <InlineError message={error} /> : null}
          <button type="submit" className="button primary" disabled={!desktopAdmin || working}>{working ? <Busy label="saving draft" /> : <><ArrowDownToLine size={15} aria-hidden="true" /> create draft</>}</button>
        </form>
        <div className="draft-list" aria-label="Saved enrollment drafts"><strong>saved drafts</strong>{drafts.length ? drafts.map((draft) => <button type="button" key={draft.id} className={draft.id === selectedId ? "draft-row selected" : "draft-row"} onClick={() => setSelectedId(draft.id)}><code>{draft.id}</code><span>{draft.display_name ?? draft.state}</span><small>{draft.blocker !== "none" ? draft.blocker : draft.execution_substate}</small></button>) : <p className="muted-line">No saved drafts.</p>}</div>
      </section>
      <section className="enrollment-progress pane" aria-labelledby="enrollment-progress-title">
        <div className="pane-title"><h2 id="enrollment-progress-title">enrollment progress</h2><span>{selected ? selected.id : "select a draft"}</span></div>
        {!selected ? <InlineEmpty title="No draft selected" detail="Create a draft on the left. The Hub stores only sanitized state; secrets stay in request memory." /> : <>
          <ol className="enrollment-stepper" aria-label="SSH enrollment steps">
            {enrollmentSteps.map(([state, label]) => <li key={state} aria-current={selected.state === state ? "step" : undefined} className={selected.state === state ? "current" : enrollmentSteps.findIndex(([candidate]) => candidate === selected.state) > enrollmentSteps.findIndex(([candidate]) => candidate === state) ? "complete" : undefined}><span className="step-icon" aria-hidden="true">{enrollmentSteps.findIndex(([candidate]) => candidate === selected.state) > enrollmentSteps.findIndex(([candidate]) => candidate === state) ? "✓" : enrollmentSteps.findIndex(([candidate]) => candidate === state) + 1}</span><span>{label}</span></li>)}
          </ol>
          <div className="enrollment-live" role="status" aria-live="polite">state: <strong>{selected.state}</strong> / {selected.execution_substate}{selected.blocker !== "none" ? ` / blocker: ${selected.blocker}` : ""}</div>
          {selected.host_fingerprint ? <div className="fingerprint-box"><FileKey2 size={16} aria-hidden="true" /><span>observed host fingerprint</span><code>{selected.host_fingerprint}</code>{selected.blocker === "unknown-host-key" ? <div className="inline-form"><label>type fingerprint to trust<input value={confirmFingerprint} onChange={(event) => setConfirmFingerprint(event.target.value)} /></label><button type="button" className="button" disabled={!desktopAdmin || working} onClick={() => void step("trust", { ...secretBody, confirm_fingerprint: confirmFingerprint })}>confirm + authenticate</button></div> : null}</div> : null}
          {selected.last_error ? <InlineError message={selected.last_error} /> : null}
          <div className="enrollment-actions desktop-admin">
            {selected.state === "target-draft" || selected.state === "host-trust-auth" ? <button type="button" className="button primary" disabled={!desktopAdmin || working} onClick={() => void step("trust", { ...secretBody, confirm_fingerprint: confirmFingerprint || undefined })}><KeyRound size={15} aria-hidden="true" /> observe host + authenticate</button> : null}
            {selected.state === "host-trust-auth" ? <button type="button" className="button" disabled={!desktopAdmin || working} onClick={() => void step("probe", secretBody)}><ArrowDownToLine size={15} aria-hidden="true" /> probe + plan</button> : null}
            {selected.state === "probe-and-plan" ? <><ArtifactFields artifact={artifact} setArtifact={setArtifact} /><button type="button" className="button primary" disabled={!desktopAdmin || working} onClick={() => void step("review", { signed_manifest: parseJson(artifact.manifest), artifact_base64: artifact.bytes, database_seed_base64: artifact.seed || undefined })}><ShieldCheck size={15} aria-hidden="true" /> verify signed plan</button></> : null}
            {selected.state === "immutable-plan-review" ? <><ArtifactFields artifact={artifact} setArtifact={setArtifact} /><button type="button" className="button primary" disabled={!desktopAdmin || working} onClick={() => void step("execute", { artifact: { signed_manifest: parseJson(artifact.manifest), artifact_base64: artifact.bytes, database_seed_base64: artifact.seed || undefined }, ...secretBody })}><UploadCloud size={15} aria-hidden="true" /> execute + verify receipt</button></> : null}
            {working ? <Busy label="Hub is advancing the durable workflow" /> : null}
          </div>
          {selected.plan ? <details className="plan-details"><summary>immutable plan {selected.plan.plan_hash}</summary><ol>{selected.plan.steps?.map((step) => <li key={step.id}>{step.description}</li>)}</ol></details> : null}
          {selected.receipt ? <div className="receipt-box"><CheckCircle2 size={17} aria-hidden="true" /><strong>receipt: {selected.receipt.status}</strong><span>Hub health {selected.receipt.hub_health_verified ? "verified" : "not verified"}; Collector {selected.receipt.collector_health_verified ? "verified" : "not verified"}</span></div> : null}
        </>}
      </section>
    </div>
  );
}

function ArtifactFields({ artifact, setArtifact }: { artifact: { manifest: string; bytes: string; seed: string }; setArtifact: (value: { manifest: string; bytes: string; seed: string }) => void }) {
  return <fieldset className="artifact-fields"><legend>signed release evidence</legend><label>signed manifest JSON<textarea value={artifact.manifest} onChange={(event) => setArtifact({ ...artifact, manifest: event.target.value })} rows={3} placeholder='{"key_id":"…","manifest":{…},"signature":"…"}' /></label><label>artifact base64<textarea value={artifact.bytes} onChange={(event) => setArtifact({ ...artifact, bytes: event.target.value })} rows={2} /></label><label>optional seed base64<textarea value={artifact.seed} onChange={(event) => setArtifact({ ...artifact, seed: event.target.value })} rows={2} /></label></fieldset>;
}

function parseJson(value: string) {
  try { return JSON.parse(value); } catch { return {}; }
}

function UpdatesTab({ csrf, desktopAdmin, updates, machines, onChange }: { csrf: string; desktopAdmin: boolean; updates: FleetUpdateRun[]; machines: MachineRecord[]; onChange: () => void }) {
  const [form, setForm] = useState({ version: "", sha256: "", key_id: "", fingerprint: "", manifest_sha256: "", manifest: "", machine_ids: "" });
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const active = updates[0] ?? null;
  const evidence = active ? { version: active.version, artifact_sha256: active.artifact_sha256, publisher_key_id: active.publisher_key_id, publisher_fingerprint: active.publisher_fingerprint, manifest_sha256: active.manifest_sha256, signature_verified: true } : null;
  async function plan(event: FormEvent) {
    event.preventDefault(); if (!desktopAdmin) return;
    setWorking(true); setError(null);
    try { await postJson("/api/v1/admin/updates/plan", { version: form.version, artifact_sha256: form.sha256, publisher_key_id: form.key_id, publisher_fingerprint: form.fingerprint, manifest_sha256: form.manifest_sha256, signed_manifest: parseJson(form.manifest), machine_ids: form.machine_ids.split(",").map((id) => id.trim()).filter(Boolean) }, csrf); onChange(); } catch (planError) { setError((planError as Error).message); } finally { setWorking(false); }
  }
  async function update(path: string, body: unknown = {}) {
    if (!active || !desktopAdmin) return;
    setWorking(true); setError(null);
    try { await postJson(`/api/v1/admin/updates/${encodeURIComponent(active.update_id)}/${path}`, body, csrf); onChange(); } catch (updateError) { setError((updateError as Error).message); } finally { setWorking(false); }
  }
  return <div className="updates-layout">
    <section className="update-plan pane" aria-labelledby="update-plan-title"><div className="pane-title"><h2 id="update-plan-title">signed fleet update</h2><span>Hub gate first / isolate nodes</span></div><form className="update-form" onSubmit={plan}><label>target version<input value={form.version} onChange={(event) => setForm({ ...form, version: event.target.value })} required /></label><label>artifact SHA-256<input value={form.sha256} onChange={(event) => setForm({ ...form, sha256: event.target.value })} minLength={64} required /></label><label>publisher key id<input value={form.key_id} onChange={(event) => setForm({ ...form, key_id: event.target.value })} required /></label><label>publisher fingerprint<input value={form.fingerprint} onChange={(event) => setForm({ ...form, fingerprint: event.target.value })} placeholder="sha256:…" required /></label><label>manifest SHA-256<input value={form.manifest_sha256} onChange={(event) => setForm({ ...form, manifest_sha256: event.target.value })} minLength={64} required /></label><label>signed manifest JSON<textarea value={form.manifest} onChange={(event) => setForm({ ...form, manifest: event.target.value })} rows={4} required /></label><label>Machine IDs <span className="muted-line">(blank = all active)</span><input value={form.machine_ids} onChange={(event) => setForm({ ...form, machine_ids: event.target.value })} placeholder={machines.map((machine) => machine.machine_id).join(", ")} /></label>{error ? <InlineError message={error} /> : null}<button type="submit" className="button primary" disabled={!desktopAdmin || working}>{working ? <Busy label="persisting plan" /> : <><FileKey2 size={15} aria-hidden="true" /> verify + plan</>}</button></form></section>
    <section className="update-run pane" aria-labelledby="update-run-title"><div className="pane-title"><h2 id="update-run-title">rollout state</h2><span>{active ? <><code>{active.version}</code> / <UpdateStatusBadge status={active.status} /> / attempt {active.attempts}</> : "no persisted run"}</span></div>{!active ? <InlineEmpty title="No update run" detail="A signed plan will show its Hub snapshot, health gate, and independent Collector receipts here." /> : <><div className="update-gate"><UpdateStep icon={<Archive size={15} aria-hidden="true" />} label="Hub snapshot" state={active.hub_snapshot_at ? "complete" : active.status === "planned" ? "ready" : "waiting"} /><UpdateStep icon={<Server size={15} aria-hidden="true" />} label="Hub update + health" state={active.hub_health_at ? "complete" : active.status === "hub-updating" ? "ready" : "waiting"} /><UpdateStep icon={<UploadCloud size={15} aria-hidden="true" />} label="Collectors" state={active.status === "collectors-queued" || active.status.startsWith("completed") ? "ready" : "waiting"} /></div><div className="update-controls desktop-admin"><button type="button" className="button" disabled={!desktopAdmin || working || active.status !== "planned"} onClick={() => void update("snapshot", evidence)}><Archive size={15} aria-hidden="true" /> snapshot Hub</button><button type="button" className="button" disabled={!desktopAdmin || working || active.status !== "hub-updating"} onClick={() => void update("health", { expected_state_revision: active.state_revision, healthy: true, restarted: true, health_checked: true, hub_version: active.version, evidence })}><ShieldCheck size={15} aria-hidden="true" /> confirm Hub health</button></div><div className="collector-rollout"><strong>Collector receipts / independent rollback</strong>{active.nodes.map((node) => <CollectorUpdateRow key={node.machine_id} node={node} update={active} desktopAdmin={desktopAdmin} working={working} evidence={evidence} onStart={() => void update(`collectors/${encodeURIComponent(node.machine_id)}/start`)} onComplete={(body) => void update(`collectors/${encodeURIComponent(node.machine_id)}/complete`, body)} />)}</div></>}</section>
  </div>;
}

function UpdateStep({ icon, label, state }: { icon: React.ReactNode; label: string; state: "complete" | "ready" | "waiting" }) { return <div className={`update-step ${state}`}><span>{icon}</span><strong>{label}</strong><StatusText state={state} /></div>; }
function StatusText({ state }: { state: string }) { return <span className="update-state"><span aria-hidden="true">{state === "complete" ? "✓" : state === "ready" ? "→" : "·"}</span> {state}</span>; }

function CollectorUpdateRow({ node, update, desktopAdmin, working, evidence, onStart, onComplete }: { node: FleetUpdateNode; update: FleetUpdateRun; desktopAdmin: boolean; working: boolean; evidence: FleetUpdateEvidence | null; onStart: () => void; onComplete: (body: unknown) => void }) {
  const [checks, setChecks] = useState({ restarted: false, health_checked: false, collector_version: update.version, protocol_version: String(node.collector_protocol_version ?? 1) });
  const canComplete = node.status === "updating" && evidence;
  return <div className="collector-update-row"><div><code>{node.machine_id}</code><span className={`update-node-status ${node.status}`}><CircleDot size={14} aria-hidden="true" /> {node.status}</span></div><small>previous {node.previous_version ?? "unknown"} / protocol {node.collector_protocol_version ?? "unknown"}</small>{node.failure_reason ? <p className="danger-text"><CircleAlert size={14} aria-hidden="true" /> {node.failure_reason}</p> : null}{node.status === "queued" ? <button type="button" className="button" disabled={!desktopAdmin || working} onClick={onStart}><ArrowUpCircle size={15} aria-hidden="true" /> snapshot + update Collector</button> : null}{canComplete ? <div className="receipt-checks"><label><input type="checkbox" checked={checks.restarted} onChange={(event) => setChecks({ ...checks, restarted: event.target.checked })} /> service restarted</label><label><input type="checkbox" checked={checks.health_checked} onChange={(event) => setChecks({ ...checks, health_checked: event.target.checked })} /> health verified</label><button type="button" className="button" disabled={!desktopAdmin || working || !checks.restarted || !checks.health_checked} onClick={() => onComplete({ expected_state_revision: node.state_revision, collector_version: checks.collector_version, protocol_version: Number(checks.protocol_version), restarted: checks.restarted, health_checked: checks.health_checked, signed_evidence: evidence })}><CheckCircle2 size={15} aria-hidden="true" /> record Collector receipt</button></div> : null}{node.status === "succeeded" ? <span className="success-text"><CheckCircle2 size={15} aria-hidden="true" /> restart, version, health, and signed evidence verified</span> : null}{node.status === "rolled-back" ? <span className="danger-text"><Undo2 size={15} aria-hidden="true" /> this node rolled back; fleet remains available</span> : null}</div>;
}

function UpdateHistory({ updates }: { updates: FleetUpdateRun[] }) { return <section className="update-history pane"><div className="pane-title"><h2>update history</h2><span>{updates.length} persisted runs</span></div>{updates.length ? <table className="machine-table"><thead><tr><th>run</th><th>version</th><th>state</th><th>Hub gate</th><th>Collectors</th><th>failure isolation</th></tr></thead><tbody>{updates.map((update) => <tr key={update.update_id}><td><code>{update.update_id}</code></td><td><code>{update.version}</code></td><td><UpdateStatusBadge status={update.status} /></td><td>{update.hub_health_at ? <span className="success-text"><CheckCircle2 size={14} aria-hidden="true" /> healthy</span> : <span className="muted-line">not passed</span>}</td><td>{update.nodes.filter((node) => node.status === "succeeded").length} passed / {update.nodes.filter((node) => node.status === "rolled-back").length} rolled back</td><td>{update.nodes.some((node) => node.status === "rolled-back") ? "fleet available; node isolated" : "none recorded"}</td></tr>)}</tbody></table> : <InlineEmpty title="No update history" detail="Signed update attempts and per-Collector rollback receipts will remain here." />}</section>; }

function formatAge(value?: string | null) { if (!value) return "never"; const time = new Date(value).getTime(); if (!Number.isFinite(time)) return "invalid timestamp"; const minutes = Math.max(0, Math.round((Date.now() - time) / 60000)); return minutes < 1 ? "now" : minutes < 60 ? `${minutes}m ago` : minutes < 1440 ? `${Math.round(minutes / 60)}h ago` : `${Math.round(minutes / 1440)}d ago`; }
function formatAbsolute(value?: string | null) { if (!value) return "—"; const date = new Date(value); return Number.isFinite(date.getTime()) ? date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }) : "invalid timestamp"; }
function compact(value: number) { return Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(value); }
