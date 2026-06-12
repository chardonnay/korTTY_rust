import type { TerminalRecordingReplayFrame } from "../types/terminalRecording";

/**
 * Replay timeline over timed frames (port of TerminalRecordingReplayTimeline.java).
 * Frame start positions are precomputed prefix sums; frame lookup by playback
 * position uses binary search.
 */

const MIN_FRAME_DURATION_SECONDS = 0.001;

export interface TerminalRecordingReplayTimeline {
  readonly frames: readonly TerminalRecordingReplayFrame[];
  readonly frameStartSeconds: readonly number[];
  readonly totalDurationSeconds: number;
}

/** Clamped frame duration; invalid or missing durations count as the minimum. */
export function frameDurationSeconds(frame: TerminalRecordingReplayFrame | null | undefined): number {
  const seconds = frame?.durationSeconds;
  if (typeof seconds !== "number" || !Number.isFinite(seconds)) {
    return MIN_FRAME_DURATION_SECONDS;
  }
  return Math.max(MIN_FRAME_DURATION_SECONDS, seconds);
}

export function createReplayTimeline(
  frames: readonly TerminalRecordingReplayFrame[] | null | undefined,
): TerminalRecordingReplayTimeline {
  const safeFrames = frames ? [...frames] : [];
  const frameStartSeconds = new Array<number>(safeFrames.length);
  let cursorSeconds = 0;
  for (let i = 0; i < safeFrames.length; i += 1) {
    frameStartSeconds[i] = cursorSeconds;
    cursorSeconds += frameDurationSeconds(safeFrames[i]);
  }
  return {
    frames: safeFrames,
    frameStartSeconds,
    totalDurationSeconds: cursorSeconds,
  };
}

export function isTimelineEmpty(timeline: TerminalRecordingReplayTimeline): boolean {
  return timeline.frames.length === 0;
}

export function totalDurationSeconds(timeline: TerminalRecordingReplayTimeline): number {
  return timeline.totalDurationSeconds;
}

/** Returns the frame at the given index, clamped into the valid index range. */
export function frameAt(
  timeline: TerminalRecordingReplayTimeline,
  index: number,
): TerminalRecordingReplayFrame {
  if (timeline.frames.length === 0) {
    throw new RangeError("No replay frames available");
  }
  const safeIndex = Math.max(0, Math.min(index, timeline.frames.length - 1));
  return timeline.frames[safeIndex];
}

export function clampSeconds(timeline: TerminalRecordingReplayTimeline, seconds: number): number {
  if (!Number.isFinite(seconds) || seconds < 0 || timeline.totalDurationSeconds <= 0) {
    return 0;
  }
  return Math.min(seconds, timeline.totalDurationSeconds);
}

/** Java's Arrays.binarySearch: index when found, -(insertionPoint) - 1 otherwise. */
function binarySearch(values: readonly number[], target: number): number {
  let low = 0;
  let high = values.length - 1;
  while (low <= high) {
    const mid = (low + high) >>> 1;
    const value = values[mid];
    if (value < target) {
      low = mid + 1;
    } else if (value > target) {
      high = mid - 1;
    } else {
      return mid;
    }
  }
  return -(low + 1);
}

export function frameIndexAt(timeline: TerminalRecordingReplayTimeline, seconds: number): number {
  if (timeline.frames.length === 0) {
    return 0;
  }
  const clampedSeconds = clampSeconds(timeline, seconds);
  if (clampedSeconds >= timeline.totalDurationSeconds) {
    return timeline.frames.length - 1;
  }
  const exactIndex = binarySearch(timeline.frameStartSeconds, clampedSeconds);
  if (exactIndex >= 0) {
    return Math.min(exactIndex, timeline.frames.length - 1);
  }
  const insertionPoint = -exactIndex - 1;
  return Math.max(0, insertionPoint - 1);
}
