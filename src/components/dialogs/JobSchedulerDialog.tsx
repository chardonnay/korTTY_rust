import { useEffect, useMemo, useState } from "react";
import { CalendarClock, KeyRound, Play, RefreshCw, Save, Square, Trash2, X } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import { useConnectionStore } from "../../store/connectionStore";
import {
  createEmptyScheduledJob,
  type HostKeyProbeResult,
  type JobAction,
  type JobActionType,
  type JobArchiveFormat,
  type JobSchedulerStatusSummary,
  type JournalEntry,
  type ScheduledJob,
} from "../../types/jobscheduler";

interface JobSchedulerDialogProps {
  open: boolean;
  onClose: () => void;
}

const JOB_ACTION_TYPES: JobActionType[] = [
  "Command",
  "SnippetScript",
  "AiAgent",
  "SftpUpload",
  "SftpDownload",
  "SftpSync",
  "SftpDelete",
  "SftpRename",
  "SftpMkdir",
  "SftpChmod",
  "SftpChown",
  "SftpCopyRemote",
  "SftpArchive",
  "RsyncSync",
];

const WEEKDAYS = ["MONDAY", "TUESDAY", "WEDNESDAY", "THURSDAY", "FRIDAY", "SATURDAY", "SUNDAY"];

function splitLines(value: string): string[] {
  return value.split(/\r?\n|,/).map((entry) => entry.trim()).filter(Boolean);
}

function joinLines(values: string[]): string {
  return values.join("\n");
}

export function JobSchedulerDialog({ open, onClose }: JobSchedulerDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("job-scheduler", 980, 700, 720, 520);
  const { connections, loadConnections } = useConnectionStore();
  const [jobs, setJobs] = useState<ScheduledJob[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<ScheduledJob | null>(null);
  const [journal, setJournal] = useState<JournalEntry[]>([]);
  const [summary, setSummary] = useState<JobSchedulerStatusSummary>({ activeJobs: [], nextRuns: [] });
  const [status, setStatus] = useState<string | null>(null);
  const [probeHost, setProbeHost] = useState("");
  const [probePort, setProbePort] = useState(22);
  const [probeResult, setProbeResult] = useState<HostKeyProbeResult | null>(null);

  useEffect(() => {
    if (!open) return;
    void loadConnections();
    void refreshAll();
    const interval = window.setInterval(() => {
      void refreshStatus();
    }, 2000);
    return () => window.clearInterval(interval);
  }, [open, loadConnections]);

  useEffect(() => {
    const selected = jobs.find((job) => job.id === selectedId) ?? null;
    setEditing(selected ? structuredClone(selected) : null);
    if (selected) {
      setProbeHost("");
      setProbePort(22);
      setProbeResult(null);
      void refreshJournal(selected.id);
    } else {
      setJournal([]);
    }
  }, [jobs, selectedId]);

  const selectedJob = useMemo(
    () => jobs.find((job) => job.id === selectedId) ?? null,
    [jobs, selectedId],
  );

  if (!open) return null;

  async function refreshAll() {
    setStatus(null);
    try {
      const [loadedJobs, loadedSummary] = await Promise.all([
        invoke<ScheduledJob[]>("get_scheduled_jobs"),
        invoke<JobSchedulerStatusSummary>("get_job_scheduler_status"),
      ]);
      setJobs(loadedJobs);
      setSummary(loadedSummary);
      setSelectedId((current) => current && loadedJobs.some((job) => job.id === current) ? current : loadedJobs[0]?.id ?? null);
    } catch (error) {
      setStatus(`Load failed: ${String(error)}`);
    }
  }

  async function refreshStatus() {
    try {
      setSummary(await invoke<JobSchedulerStatusSummary>("get_job_scheduler_status"));
    } catch (error) {
      console.error("Failed to load JobScheduler status:", error);
    }
  }

  async function refreshJournal(jobId = selectedId ?? undefined) {
    try {
      setJournal(await invoke<JournalEntry[]>("get_job_journal", { jobId }));
    } catch (error) {
      setStatus(`Journal load failed: ${String(error)}`);
    }
  }

  function updateJob(partial: Partial<ScheduledJob>) {
    setEditing((current) => current ? { ...current, ...partial, updatedAt: new Date().toISOString() } : current);
  }

  function updateAction(partial: Partial<JobAction>) {
    setEditing((current) => current
      ? { ...current, action: { ...current.action, ...partial }, updatedAt: new Date().toISOString() }
      : current);
  }

  async function saveJob() {
    if (!editing) return;
    try {
      const saved = await invoke<ScheduledJob>("save_scheduled_job", { job: editing });
      setStatus(`Saved "${saved.name || "Unnamed job"}".`);
      await refreshAll();
      setSelectedId(saved.id);
    } catch (error) {
      setStatus(`Save failed: ${String(error)}`);
    }
  }

  async function runJob() {
    if (!selectedJob) return;
    try {
      const runId = await invoke<string>("run_scheduled_job_now", { jobId: selectedJob.id });
      setStatus(`Started run ${runId}.`);
      await refreshStatus();
    } catch (error) {
      setStatus(`Run failed: ${String(error)}`);
    }
  }

  async function deleteJob() {
    if (!selectedJob) return;
    try {
      await invoke("delete_scheduled_job", { id: selectedJob.id });
      setStatus(`Deleted "${selectedJob.name || "Unnamed job"}".`);
      await refreshAll();
    } catch (error) {
      setStatus(`Delete failed: ${String(error)}`);
    }
  }

  async function cancelRun(runId: string) {
    try {
      await invoke("cancel_scheduled_job_run", { runId });
      await refreshStatus();
    } catch (error) {
      setStatus(`Cancel failed: ${String(error)}`);
    }
  }

  async function probeHostKey() {
    if (!probeHost.trim()) return;
    try {
      setProbeResult(await invoke<HostKeyProbeResult>("probe_job_host_key", { host: probeHost.trim(), port: probePort }));
    } catch (error) {
      setStatus(`Host key probe failed: ${String(error)}`);
    }
  }

  async function pinHostKey() {
    if (!probeResult) return;
    try {
      await invoke("pin_job_host_key", { probe: probeResult, source: "job-scheduler-ui" });
      setStatus(`Pinned ${probeResult.host}:${probeResult.port}.`);
    } catch (error) {
      setStatus(`Pin failed: ${String(error)}`);
    }
  }

  const activeJobIds = new Set(summary.activeJobs.map((active) => active.jobId));

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/50">
      <div
        className="relative flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width, height, maxWidth: "96vw", maxHeight: "96vh" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold">
            <CalendarClock className="h-4 w-4 text-kortty-accent" />
            JobScheduler
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose}>
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex min-h-0 flex-1">
          <div className="w-72 shrink-0 overflow-y-auto border-r border-kortty-border p-2">
            <div className="mb-2 flex gap-2">
              <button
                className="btn-primary flex-1 text-xs"
                onClick={() => {
                  const job = createEmptyScheduledJob();
                  setJobs((current) => [job, ...current]);
                  setSelectedId(job.id);
                }}
              >
                New
              </button>
              <button className="btn-secondary text-xs" onClick={() => void refreshAll()}>
                <RefreshCw className="h-3.5 w-3.5" />
              </button>
            </div>
            {jobs.map((job) => (
              <button
                key={job.id}
                className={`mb-1 w-full rounded px-3 py-2 text-left text-xs ${
                  selectedId === job.id ? "bg-kortty-accent/10 text-kortty-accent" : "hover:bg-kortty-panel"
                }`}
                onClick={() => setSelectedId(job.id)}
              >
                <div className="flex items-center gap-2">
                  <span className="min-w-0 flex-1 truncate font-medium">{job.name || "Unnamed job"}</span>
                  {activeJobIds.has(job.id) && <span className="font-mono text-kortty-success">RUN</span>}
                </div>
                <div className="truncate text-[11px] text-kortty-text-dim">{job.action.actionType}</div>
                <div className="truncate text-[11px] text-kortty-text-dim">
                  {job.nextRunAt ? `Next: ${new Date(job.nextRunAt).toLocaleString()}` : "No queued run"}
                </div>
              </button>
            ))}
          </div>

          <div className="flex min-w-0 flex-1 flex-col">
            <div className="flex items-center gap-2 border-b border-kortty-border px-3 py-2">
              <button className="btn-primary flex items-center gap-2 text-xs" disabled={!editing} onClick={() => void saveJob()}>
                <Save className="h-3.5 w-3.5" /> Save
              </button>
              <button className="btn-secondary flex items-center gap-2 text-xs" disabled={!selectedJob} onClick={() => void runJob()}>
                <Play className="h-3.5 w-3.5" /> Run now
              </button>
              <button className="btn-secondary flex items-center gap-2 text-xs text-kortty-error" disabled={!selectedJob} onClick={() => void deleteJob()}>
                <Trash2 className="h-3.5 w-3.5" /> Delete
              </button>
              <div className="ml-auto text-[11px] text-kortty-text-dim">
                {summary.activeJobs.length} active · {summary.nextRuns.length} queued
              </div>
            </div>

            <div className="grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)_360px] overflow-hidden">
              <div className="overflow-y-auto p-4">
                {editing ? (
                  <JobEditor
                    job={editing}
                    connections={connections}
                    updateJob={updateJob}
                    updateAction={updateAction}
                  />
                ) : (
                  <div className="py-10 text-center text-xs text-kortty-text-dim">Select or create a job.</div>
                )}
              </div>

              <div className="overflow-y-auto border-l border-kortty-border p-3 text-xs">
                <div className="mb-3 rounded border border-kortty-border bg-kortty-panel/30 p-2">
                  <div className="mb-2 flex items-center gap-2 font-medium">
                    <KeyRound className="h-3.5 w-3.5 text-kortty-accent" />
                    Host key pinning
                  </div>
                  <input className="input-field mb-2" placeholder="host" value={probeHost} onChange={(event) => setProbeHost(event.target.value)} />
                  <input className="input-field mb-2" type="number" min={1} max={65535} value={probePort} onChange={(event) => setProbePort(Number(event.target.value) || 22)} />
                  <div className="flex gap-2">
                    <button className="btn-secondary flex-1 text-xs" onClick={() => void probeHostKey()}>Probe</button>
                    <button className="btn-primary flex-1 text-xs" disabled={!probeResult} onClick={() => void pinHostKey()}>Pin</button>
                  </div>
                  {probeResult && (
                    <div className="mt-2 break-all font-mono text-[10px] text-kortty-text-dim">
                      {probeResult.algorithm} {probeResult.sha256Fingerprint}
                    </div>
                  )}
                </div>

                <div className="mb-3">
                  <div className="mb-2 font-medium">Active runs</div>
                  {summary.activeJobs.map((active) => (
                    <div key={active.runId} className="mb-1 rounded border border-kortty-border bg-kortty-panel/30 p-2">
                      <div className="truncate">{active.jobName}</div>
                      <div className="text-[11px] text-kortty-text-dim">{active.status} · {new Date(active.startedAt).toLocaleTimeString()}</div>
                      <button className="mt-2 btn-secondary flex items-center gap-2 text-xs" onClick={() => void cancelRun(active.runId)}>
                        <Square className="h-3.5 w-3.5" /> Cancel
                      </button>
                    </div>
                  ))}
                  {summary.activeJobs.length === 0 && <div className="text-kortty-text-dim">No active runs.</div>}
                </div>

                <div>
                  <div className="mb-2 flex items-center justify-between">
                    <span className="font-medium">Journal</span>
                    <button className="text-kortty-accent hover:underline" onClick={() => void refreshJournal()}>Refresh</button>
                  </div>
                  {journal.map((entry) => (
                    <details key={entry.id} className="mb-1 rounded border border-kortty-border bg-kortty-panel/30 p-2">
                      <summary className="cursor-pointer text-[11px]">
                        {entry.status} · {new Date(entry.startedAt).toLocaleString()}
                      </summary>
                      <div className="mt-1 whitespace-pre-wrap text-[11px] text-kortty-text-dim">{entry.summary}</div>
                      {entry.detail && <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap text-[10px]">{entry.detail}</pre>}
                    </details>
                  ))}
                  {journal.length === 0 && <div className="text-kortty-text-dim">No journal entries.</div>}
                </div>
              </div>
            </div>
          </div>
        </div>

        {status && <div className="border-t border-kortty-border px-4 py-2 text-xs text-kortty-text-dim">{status}</div>}
        <div className="absolute bottom-0 right-0 h-4 w-4 cursor-nwse-resize opacity-40 hover:opacity-100" onMouseDown={onResizeStart} />
      </div>
    </div>
  );
}

function JobEditor({
  job,
  connections,
  updateJob,
  updateAction,
}: {
  job: ScheduledJob;
  connections: ReturnType<typeof useConnectionStore.getState>["connections"];
  updateJob: (partial: Partial<ScheduledJob>) => void;
  updateAction: (partial: Partial<JobAction>) => void;
}) {
  const selectedTargets = new Set(job.targetConnectionIds);
  const action = job.action;

  function toggleTarget(connectionId: string) {
    const next = new Set(selectedTargets);
    if (next.has(connectionId)) next.delete(connectionId);
    else next.add(connectionId);
    updateJob({ targetConnectionIds: Array.from(next), connectionId: Array.from(next)[0] });
  }

  return (
    <div className="space-y-4 text-xs">
      <section className="grid grid-cols-2 gap-3">
        <Field label="Name">
          <input className="input-field" value={job.name} onChange={(event) => updateJob({ name: event.target.value })} />
        </Field>
        <Field label="Journal detail">
          <select className="input-field" value={job.journalDetailMode} onChange={(event) => updateJob({ journalDetailMode: event.target.value as ScheduledJob["journalDetailMode"] })}>
            <option value="LimitedRedacted">Limited redacted</option>
            <option value="Full">Full</option>
          </select>
        </Field>
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={job.enabled} onChange={(event) => updateJob({ enabled: event.target.checked })} />
          Enabled
        </label>
        <label className="flex items-center gap-2 text-amber-300">
          <input type="checkbox" checked={job.hostKeyVerificationDisabled} onChange={(event) => updateJob({ hostKeyVerificationDisabled: event.target.checked })} />
          Disable host-key verification for this job
        </label>
      </section>

      <section>
        <div className="mb-2 font-medium">Targets</div>
        <div className="grid grid-cols-2 gap-2">
          {connections.map((connection) => (
            <label key={connection.id} className="flex items-center gap-2 rounded border border-kortty-border bg-kortty-panel/30 px-2 py-1">
              <input type="checkbox" checked={selectedTargets.has(connection.id)} onChange={() => toggleTarget(connection.id)} />
              <span className="min-w-0 flex-1 truncate">{connection.name || `${connection.username}@${connection.host}`}</span>
              {connection.connectionProtocol === "Mosh" && <span className="text-[10px] text-amber-300">Mosh blocked</span>}
            </label>
          ))}
        </div>
      </section>

      <section className="grid grid-cols-2 gap-3">
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={job.schedule.enabled} onChange={(event) => updateJob({ schedule: { ...job.schedule, enabled: event.target.checked } })} />
          Schedule enabled
        </label>
        <Field label="Interval minutes">
          <input className="input-field" type="number" value={job.schedule.intervalMinutes ?? ""} onChange={(event) => updateJob({ schedule: { ...job.schedule, intervalMinutes: event.target.value ? Number(event.target.value) : undefined } })} />
        </Field>
        <Field label="Fixed times (comma or newline)">
          <textarea className="input-field min-h-20" value={joinLines(job.schedule.fixedTimes)} onChange={(event) => updateJob({ schedule: { ...job.schedule, fixedTimes: splitLines(event.target.value) } })} />
        </Field>
        <div>
          <div className="mb-1 text-[10px] text-kortty-text-dim">Weekdays</div>
          <div className="grid grid-cols-2 gap-1">
            {WEEKDAYS.map((day) => (
              <label key={day} className="flex items-center gap-1">
                <input
                  type="checkbox"
                  checked={job.schedule.weekdays.includes(day)}
                  onChange={(event) => {
                    const next = new Set(job.schedule.weekdays);
                    if (event.target.checked) next.add(day);
                    else next.delete(day);
                    updateJob({ schedule: { ...job.schedule, weekdays: Array.from(next) } });
                  }}
                />
                {day.slice(0, 3)}
              </label>
            ))}
          </div>
        </div>
        <Field label="Active from date"><input className="input-field" type="date" value={job.schedule.activeFromDate ?? ""} onChange={(event) => updateJob({ schedule: { ...job.schedule, activeFromDate: event.target.value || undefined } })} /></Field>
        <Field label="Active until date"><input className="input-field" type="date" value={job.schedule.activeUntilDate ?? ""} onChange={(event) => updateJob({ schedule: { ...job.schedule, activeUntilDate: event.target.value || undefined } })} /></Field>
        <Field label="Window start"><input className="input-field" type="time" value={job.schedule.windowStartTime ?? ""} onChange={(event) => updateJob({ schedule: { ...job.schedule, windowStartTime: event.target.value || undefined } })} /></Field>
        <Field label="Window end"><input className="input-field" type="time" value={job.schedule.windowEndTime ?? ""} onChange={(event) => updateJob({ schedule: { ...job.schedule, windowEndTime: event.target.value || undefined } })} /></Field>
      </section>

      <section className="space-y-3">
        <Field label="Action type">
          <select className="input-field" value={action.actionType} onChange={(event) => updateAction({ actionType: event.target.value as JobActionType })}>
            {JOB_ACTION_TYPES.map((type) => <option key={type} value={type}>{type}</option>)}
          </select>
        </Field>
        <ActionFields action={action} updateAction={updateAction} />
        <div className="grid grid-cols-2 gap-3">
          <Field label="Working directory">
            <input className="input-field" value={job.workingDirectory ?? ""} onChange={(event) => updateJob({ workingDirectory: event.target.value || undefined })} />
          </Field>
          <div className="flex items-end gap-4">
            <label className="flex items-center gap-2"><input type="checkbox" checked={action.useSudo} onChange={(event) => updateAction({ useSudo: event.target.checked })} /> Use sudo</label>
            <label className="flex items-center gap-2"><input type="checkbox" checked={action.sudoStagingEnabled} onChange={(event) => updateAction({ sudoStagingEnabled: event.target.checked })} /> Sudo staging</label>
          </div>
        </div>
      </section>
    </div>
  );
}

function ActionFields({ action, updateAction }: { action: JobAction; updateAction: (partial: Partial<JobAction>) => void }) {
  switch (action.actionType) {
    case "Command":
      return <Field label="Command"><textarea className="input-field min-h-28 font-mono" value={action.command ?? ""} onChange={(event) => updateAction({ command: event.target.value })} /></Field>;
    case "SnippetScript":
      return (
        <div className="grid grid-cols-2 gap-3">
          <Field label="Snippet ID"><input className="input-field" value={action.snippetId ?? ""} onChange={(event) => updateAction({ snippetId: event.target.value || undefined })} /></Field>
          <Field label="Arguments"><input className="input-field" value={action.snippetArguments.join(" ")} onChange={(event) => updateAction({ snippetArguments: splitLines(event.target.value.replace(/\s+/g, ",")) })} /></Field>
        </div>
      );
    case "AiAgent":
      return (
        <div className="space-y-3">
          <Field label="AI profile ID"><input className="input-field" value={action.aiProfileId ?? ""} onChange={(event) => updateAction({ aiProfileId: event.target.value || undefined })} /></Field>
          <Field label="AI prompt"><textarea className="input-field min-h-28" value={action.aiPrompt ?? ""} onChange={(event) => updateAction({ aiPrompt: event.target.value || undefined })} /></Field>
          <label className="flex items-center gap-2"><input type="checkbox" checked={action.aiAutoApproveCommands} onChange={(event) => updateAction({ aiAutoApproveCommands: event.target.checked })} /> Auto approve commands</label>
        </div>
      );
    case "SftpUpload":
    case "SftpDownload":
      return <PathPair action={action} updateAction={updateAction} />;
    case "SftpSync":
      return (
        <div className="grid grid-cols-3 gap-3">
          <Field label="Direction"><select className="input-field" value={action.syncDirection ?? "Upload"} onChange={(event) => updateAction({ syncDirection: event.target.value as JobAction["syncDirection"] })}><option value="Upload">Upload</option><option value="Download">Download</option></select></Field>
          <Field label="Local path"><input className="input-field" value={action.localPath ?? ""} onChange={(event) => updateAction({ localPath: event.target.value || undefined })} /></Field>
          <Field label="Remote path"><input className="input-field" value={action.remotePath ?? ""} onChange={(event) => updateAction({ remotePath: event.target.value || undefined })} /></Field>
        </div>
      );
    case "SftpDelete":
    case "SftpMkdir":
      return <Field label="Remote path"><input className="input-field" value={action.remotePath ?? ""} onChange={(event) => updateAction({ remotePath: event.target.value || undefined })} /></Field>;
    case "SftpRename":
      return <RemoteSourceDest action={action} updateAction={updateAction} newName />;
    case "SftpChmod":
      return <RemoteExtra action={action} updateAction={updateAction} extra="permissions" />;
    case "SftpChown":
      return <RemoteExtra action={action} updateAction={updateAction} extra="owner" />;
    case "SftpCopyRemote":
      return <RemoteSourceDest action={action} updateAction={updateAction} />;
    case "SftpArchive":
      return <ArchiveFields action={action} updateAction={updateAction} />;
    case "RsyncSync":
      return <RsyncFields action={action} updateAction={updateAction} />;
  }
}

function PathPair({ action, updateAction }: { action: JobAction; updateAction: (partial: Partial<JobAction>) => void }) {
  return (
    <div className="grid grid-cols-2 gap-3">
      <Field label="Local path"><input className="input-field" value={action.localPath ?? ""} onChange={(event) => updateAction({ localPath: event.target.value || undefined })} /></Field>
      <Field label="Remote path"><input className="input-field" value={action.remotePath ?? ""} onChange={(event) => updateAction({ remotePath: event.target.value || undefined })} /></Field>
    </div>
  );
}

function RemoteSourceDest({ action, updateAction, newName = false }: { action: JobAction; updateAction: (partial: Partial<JobAction>) => void; newName?: boolean }) {
  return (
    <div className="grid grid-cols-2 gap-3">
      <Field label="Remote source"><input className="input-field" value={action.remoteSourcePath ?? ""} onChange={(event) => updateAction({ remoteSourcePath: event.target.value || undefined })} /></Field>
      <Field label={newName ? "New name" : "Remote destination"}><input className="input-field" value={newName ? action.newName ?? "" : action.remoteDestinationPath ?? ""} onChange={(event) => updateAction(newName ? { newName: event.target.value || undefined } : { remoteDestinationPath: event.target.value || undefined })} /></Field>
    </div>
  );
}

function RemoteExtra({ action, updateAction, extra }: { action: JobAction; updateAction: (partial: Partial<JobAction>) => void; extra: "permissions" | "owner" }) {
  return (
    <div className="grid grid-cols-3 gap-3">
      <Field label="Remote path"><input className="input-field" value={action.remotePath ?? ""} onChange={(event) => updateAction({ remotePath: event.target.value || undefined })} /></Field>
      <Field label={extra === "permissions" ? "Permissions" : "Owner"}><input className="input-field" value={extra === "permissions" ? action.permissions ?? "" : action.owner ?? ""} onChange={(event) => updateAction(extra === "permissions" ? { permissions: event.target.value || undefined } : { owner: event.target.value || undefined })} /></Field>
      {extra === "owner" && <Field label="Group"><input className="input-field" value={action.group ?? ""} onChange={(event) => updateAction({ group: event.target.value || undefined })} /></Field>}
    </div>
  );
}

function ArchiveFields({ action, updateAction }: { action: JobAction; updateAction: (partial: Partial<JobAction>) => void }) {
  return (
    <div className="grid grid-cols-2 gap-3">
      <Field label="Source paths"><textarea className="input-field min-h-20" value={joinLines(action.archiveSourcePaths)} onChange={(event) => updateAction({ archiveSourcePaths: splitLines(event.target.value) })} /></Field>
      <Field label="Exclude patterns"><textarea className="input-field min-h-20" value={joinLines(action.archiveExcludePatterns)} onChange={(event) => updateAction({ archiveExcludePatterns: splitLines(event.target.value) })} /></Field>
      <Field label="Archive path"><input className="input-field" value={action.archivePath ?? ""} onChange={(event) => updateAction({ archivePath: event.target.value || undefined })} /></Field>
      <Field label="Format"><select className="input-field" value={action.archiveFormat ?? "Zip"} onChange={(event) => updateAction({ archiveFormat: event.target.value as JobArchiveFormat })}><option value="Zip">ZIP</option><option value="ZipPassword">ZIP password</option><option value="Tar">TAR</option><option value="TarBz2">TAR BZ2</option></select></Field>
      <Field label="Compression"><input className="input-field" type="number" min={0} max={9} value={action.archiveCompressionLevel ?? 6} onChange={(event) => updateAction({ archiveCompressionLevel: Number(event.target.value) || 0 })} /></Field>
      <Field label="Download local path"><input className="input-field" value={action.archiveDownloadLocalPath ?? ""} onChange={(event) => updateAction({ archiveDownloadLocalPath: event.target.value || undefined })} /></Field>
      <label className="flex items-center gap-2"><input type="checkbox" checked={action.archiveDownloadAfterCreate} onChange={(event) => updateAction({ archiveDownloadAfterCreate: event.target.checked })} /> Download after create</label>
    </div>
  );
}

function RsyncFields({ action, updateAction }: { action: JobAction; updateAction: (partial: Partial<JobAction>) => void }) {
  return (
    <div className="grid grid-cols-2 gap-3">
      <Field label="Direction"><select className="input-field" value={action.rsyncDirection ?? "Upload"} onChange={(event) => updateAction({ rsyncDirection: event.target.value as JobAction["rsyncDirection"] })}><option value="Upload">Upload</option><option value="Download">Download</option></select></Field>
      <Field label="Target root"><input className="input-field" value={action.rsyncTargetRoot ?? ""} onChange={(event) => updateAction({ rsyncTargetRoot: event.target.value || undefined })} /></Field>
      <Field label="Source paths"><textarea className="input-field min-h-20" value={joinLines(action.rsyncSourcePaths)} onChange={(event) => updateAction({ rsyncSourcePaths: splitLines(event.target.value) })} /></Field>
      <label className="flex items-center gap-2"><input type="checkbox" checked={action.rsyncDeleteEnabled} onChange={(event) => updateAction({ rsyncDeleteEnabled: event.target.checked })} /> Delete extraneous files</label>
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="mb-1 block text-[10px] text-kortty-text-dim">{label}</label>
      {children}
    </div>
  );
}
