export type TerminalRecordingScope = "ActiveSplit" | "WholeTab";
export type TerminalRecordingState = "Idle" | "Recording" | "Paused" | "AutoPaused" | "Stopped";
export type TerminalRecordingExportFormat = "Webm" | "Mkv";
export type TerminalRecordingFormat = "KorttyReplay" | "Webm";

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

export interface TerminalRecordingStyleRun {
  row: number;
  column: number;
  text: string;
  foreground?: string;
  background?: string;
  options?: string[];
}

export interface TerminalRecordingReplayEvent {
  eventType: string;
  atMillis: number;
  text?: string;
  columns?: number;
  rows?: number;
  cursorColumn?: number;
  cursorRow?: number;
  widget?: string;
  pixelWidth?: number;
  pixelHeight?: number;
  styleRuns?: TerminalRecordingStyleRun[];
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

export interface TerminalRecordingReplayFrame {
  content: string;
  columns: number;
  rows: number;
  pixelWidth: number;
  pixelHeight: number;
  styleRuns: TerminalRecordingStyleRun[];
  durationSeconds: number;
}

export interface TerminalRecordingReplayFrames {
  frames: TerminalRecordingReplayFrame[];
  totalDurationSeconds: number;
}

/** Payload of the "kortty-recording-state" backend event. */
export interface TerminalRecordingStateEvent {
  sessionId: string;
  tabId: string;
  splitId?: string;
  state: TerminalRecordingState;
}
