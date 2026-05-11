export type JobActionType =
  | "Command"
  | "SnippetScript"
  | "AiAgent"
  | "SftpUpload"
  | "SftpDownload"
  | "SftpSync"
  | "SftpDelete"
  | "SftpRename"
  | "SftpMkdir"
  | "SftpChmod"
  | "SftpChown"
  | "SftpCopyRemote"
  | "SftpArchive"
  | "RsyncSync";

export type JobRunStatus = "Running" | "Success" | "Failed" | "Blocked" | "Cancelled";
export type JournalDetailMode = "LimitedRedacted" | "Full";
export type RsyncDirection = "Upload" | "Download";
export type SftpSyncDirection = "Upload" | "Download";
export type JobArchiveFormat = "Zip" | "ZipPassword" | "Tar" | "TarBz2";

export interface JobSchedule {
  enabled: boolean;
  weekdays: string[];
  fixedTimes: string[];
  activeFromDate?: string;
  activeUntilDate?: string;
  windowStartTime?: string;
  windowEndTime?: string;
  intervalMinutes?: number;
}

export interface JobAction {
  actionType: JobActionType;
  command?: string;
  snippetId?: string;
  snippetArguments: string[];
  aiPrompt?: string;
  aiProfileId?: string;
  aiAutoApproveCommands: boolean;
  localPath?: string;
  remotePath?: string;
  remoteSourcePath?: string;
  remoteDestinationPath?: string;
  newName?: string;
  permissions?: string;
  owner?: string;
  group?: string;
  syncDirection?: SftpSyncDirection;
  useSudo: boolean;
  sudoStagingEnabled: boolean;
  archiveSourcePaths: string[];
  archiveExcludePatterns: string[];
  archivePath?: string;
  archiveFormat?: JobArchiveFormat;
  archiveCompressionLevel?: number;
  archiveDownloadAfterCreate: boolean;
  archiveDownloadLocalPath?: string;
  encryptedArchivePassword?: string;
  rsyncDirection?: RsyncDirection;
  rsyncSourcePaths: string[];
  rsyncTargetRoot?: string;
  rsyncDeleteEnabled: boolean;
}

export interface ScheduledJob {
  id: string;
  name: string;
  enabled: boolean;
  hostKeyVerificationDisabled: boolean;
  connectionId?: string;
  connectionDisplayName?: string;
  targetConnectionIds: string[];
  targetGroupNames: string[];
  workingDirectory?: string;
  journalDetailMode: JournalDetailMode;
  schedule: JobSchedule;
  action: JobAction;
  lastRunAt?: string;
  nextRunAt?: string;
  createdAt: string;
  updatedAt: string;
}

export interface JournalEntry {
  id: string;
  runId: string;
  jobId: string;
  targetConnectionId?: string;
  targetDisplayName?: string;
  status: JobRunStatus;
  startedAt: string;
  finishedAt?: string;
  summary: string;
  detail: string;
}

export interface PinnedHostKey {
  id: string;
  host: string;
  port: number;
  algorithm: string;
  sha256Fingerprint: string;
  opensshPublicKey?: string;
  pinnedAt: string;
  source?: string;
}

export interface HostKeyProbeResult {
  host: string;
  port: number;
  algorithm: string;
  sha256Fingerprint: string;
  opensshPublicKey: string;
}

export interface ActiveJobSummary {
  runId: string;
  jobId: string;
  jobName: string;
  status: JobRunStatus;
  targetDisplayName?: string;
  startedAt: string;
  cancellable: boolean;
}

export interface JobSchedulerStatusSummary {
  activeJobs: ActiveJobSummary[];
  nextRuns: ScheduledJob[];
}

export function createEmptyScheduledJob(): ScheduledJob {
  const now = new Date().toISOString();
  return {
    id: crypto.randomUUID(),
    name: "",
    enabled: true,
    hostKeyVerificationDisabled: false,
    targetConnectionIds: [],
    targetGroupNames: [],
    journalDetailMode: "LimitedRedacted",
    schedule: {
      enabled: false,
      weekdays: [],
      fixedTimes: [],
      intervalMinutes: undefined,
    },
    action: {
      actionType: "Command",
      snippetArguments: [],
      aiAutoApproveCommands: false,
      useSudo: false,
      sudoStagingEnabled: false,
      archiveSourcePaths: [],
      archiveExcludePatterns: [],
      archiveCompressionLevel: 6,
      archiveDownloadAfterCreate: false,
      rsyncSourcePaths: [],
      rsyncDeleteEnabled: false,
    },
    createdAt: now,
    updatedAt: now,
  };
}
