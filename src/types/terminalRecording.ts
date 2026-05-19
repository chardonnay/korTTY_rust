export type TerminalRecordingScope = "ActiveSplit" | "WholeTab";
export type TerminalRecordingState = "Recording" | "Paused" | "Stopped";
export type TerminalRecordingExportFormat = "Webm" | "Mkv";

export interface TerminalRecordingStartRequest {
  tabId: string;
  splitId?: string;
  connectionName?: string;
  scope: TerminalRecordingScope;
  columns: number;
  rows: number;
}

export interface TerminalRecordingStartResponse {
  sessionId: string;
  filePath: string;
  state: TerminalRecordingState;
  startedAtMillis: number;
}

export interface TerminalRecordingReplayEvent {
  eventType: string;
  atMillis: number;
  text?: string;
  columns?: number;
  rows?: number;
  cursorColumn?: number;
  cursorRow?: number;
}

export interface TerminalRecordingReplaySummary {
  id: string;
  name: string;
  filePath: string;
  sizeBytes: number;
  compressed: boolean;
  startedAtMillis?: number;
  endedAtMillis?: number;
  durationMillis?: number;
  eventCount: number;
}

export interface TerminalRecordingReplayFile {
  summary: TerminalRecordingReplaySummary;
  events: TerminalRecordingReplayEvent[];
}
