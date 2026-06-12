import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Clapperboard, Pause, Play, Square, X } from "lucide-react";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import {
  clampSeconds,
  createReplayTimeline,
  frameAt,
  frameIndexAt,
  isTimelineEmpty,
} from "../../utils/terminalRecordingTimeline";
import { parseTimeJumpSeconds } from "../../utils/terminalRecordingTimeJump";
import type {
  TerminalRecordingReplayFrame,
  TerminalRecordingReplayFrames,
  TerminalRecordingStyleRun,
} from "../../types/terminalRecording";

/** Port of TerminalRecordingManagerDialog.formatDuration / TerminalRecordingReplayDialog.formatDuration (Java). */
export function formatRecordingDuration(seconds: number): string {
  const rounded = Math.max(0, Math.round(Number.isFinite(seconds) ? seconds : 0));
  const hours = Math.floor(rounded / 3600);
  const minutes = Math.floor((rounded % 3600) / 60);
  const rest = rounded % 60;
  if (hours > 0) {
    return `${hours}:${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
  }
  return `${minutes}:${String(rest).padStart(2, "0")}`;
}

function styleForRun(run: TerminalRecordingStyleRun): CSSProperties {
  const options = run.options ?? [];
  let foreground = run.foreground;
  let background = run.background;
  if (options.includes("INVERSE")) {
    [foreground, background] = [background, foreground];
  }
  const style: CSSProperties = {};
  if (foreground) style.color = foreground;
  if (background) style.backgroundColor = background;
  if (options.includes("BOLD")) style.fontWeight = "bold";
  if (options.includes("ITALIC")) style.fontStyle = "italic";
  if (options.includes("UNDERLINE") || options.includes("UNDERLINED")) style.textDecoration = "underline";
  if (options.includes("DIM")) style.opacity = 0.7;
  if (options.includes("HIDDEN")) style.visibility = "hidden";
  return style;
}

/** Renders frame content with inline color spans when style runs are present, plain text otherwise. */
function renderFrameContent(frame: TerminalRecordingReplayFrame): ReactNode {
  const styleRuns = frame.styleRuns ?? [];
  if (styleRuns.length === 0) {
    return frame.content;
  }
  const lines = frame.content.split("\n");
  const runsByRow = new Map<number, TerminalRecordingStyleRun[]>();
  for (const run of styleRuns) {
    const row = run.row ?? 0;
    const existing = runsByRow.get(row);
    if (existing) {
      existing.push(run);
    } else {
      runsByRow.set(row, [run]);
    }
  }
  const nodes: ReactNode[] = [];
  lines.forEach((line, rowIndex) => {
    const rowRuns = (runsByRow.get(rowIndex) ?? []).slice().sort((a, b) => (a.column ?? 0) - (b.column ?? 0));
    if (rowRuns.length === 0) {
      nodes.push(line);
    } else {
      let cursor = 0;
      rowRuns.forEach((run, runIndex) => {
        const text = run.text ?? "";
        const column = Math.max(0, run.column ?? 0);
        if (column < cursor || text.length === 0) {
          return;
        }
        if (column > cursor) {
          nodes.push(line.slice(cursor, column));
        }
        nodes.push(
          <span key={`run-${rowIndex}-${runIndex}`} style={styleForRun(run)}>
            {text}
          </span>,
        );
        cursor = column + text.length;
      });
      if (cursor < line.length) {
        nodes.push(line.slice(cursor));
      }
    }
    if (rowIndex < lines.length - 1) {
      nodes.push("\n");
    }
  });
  return nodes;
}

interface TerminalRecordingReplayDialogProps {
  open: boolean;
  onClose: () => void;
  replayPath: string;
  replayName: string;
}

type ReplayLoadState = "loading" | "ready" | "error";

/** Replay viewer dialog (port of TerminalRecordingReplayDialog.java). */
export function TerminalRecordingReplayDialog({
  open,
  onClose,
  replayPath,
  replayName,
}: TerminalRecordingReplayDialogProps) {
  const { t } = useTranslation();
  const { width, height, onResizeStart } = useDialogGeometry("terminal-recording-replay", 1040, 720, 640, 420);

  const [loadState, setLoadState] = useState<ReplayLoadState>("loading");
  const [loadError, setLoadError] = useState<string | null>(null);
  const [frames, setFrames] = useState<TerminalRecordingReplayFrame[]>([]);
  const [playing, setPlaying] = useState(false);
  const [positionSeconds, setPositionSeconds] = useState(0);
  const [frameIndex, setFrameIndex] = useState(0);
  const [speed, setSpeed] = useState(1);
  const [timeJumpText, setTimeJumpText] = useState("");
  const [statusMessage, setStatusMessage] = useState<string | null>(null);

  const rafRef = useRef<number | null>(null);
  const playingRef = useRef(false);
  const speedRef = useRef(1);
  const positionRef = useRef(0);
  const frameIndexRef = useRef(-1);
  const clockRef = useRef({ startedAtMillis: 0, startPositionSeconds: 0 });
  const sliderSeekingRef = useRef(false);
  const resumeAfterSliderSeekRef = useRef(false);

  const timeline = useMemo(() => createReplayTimeline(frames), [frames]);
  const timelineEmpty = isTimelineEmpty(timeline);

  const cancelAnimation = useCallback(() => {
    if (rafRef.current != null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
  }, []);

  const applyPosition = useCallback(
    (seconds: number, forceFrameUpdate: boolean) => {
      setStatusMessage(null);
      if (isTimelineEmpty(timeline)) {
        positionRef.current = 0;
        frameIndexRef.current = 0;
        setPositionSeconds(0);
        setFrameIndex(0);
        return;
      }
      const clamped = clampSeconds(timeline, seconds);
      const nextIndex = frameIndexAt(timeline, clamped);
      positionRef.current = clamped;
      setPositionSeconds(clamped);
      if (forceFrameUpdate || nextIndex !== frameIndexRef.current) {
        frameIndexRef.current = nextIndex;
        setFrameIndex(nextIndex);
      }
    },
    [timeline],
  );

  const pausePlayback = useCallback(() => {
    playingRef.current = false;
    setPlaying(false);
    cancelAnimation();
  }, [cancelAnimation]);

  const tick = useCallback(() => {
    if (!playingRef.current || isTimelineEmpty(timeline)) {
      return;
    }
    const elapsedSeconds =
      ((performance.now() - clockRef.current.startedAtMillis) / 1000) * speedRef.current;
    const targetSeconds = clockRef.current.startPositionSeconds + elapsedSeconds;
    if (targetSeconds >= timeline.totalDurationSeconds) {
      playingRef.current = false;
      setPlaying(false);
      cancelAnimation();
      applyPosition(timeline.totalDurationSeconds, true);
      return;
    }
    applyPosition(targetSeconds, false);
    rafRef.current = requestAnimationFrame(tick);
  }, [timeline, applyPosition, cancelAnimation]);

  const startPlayback = useCallback(() => {
    if (isTimelineEmpty(timeline)) {
      return;
    }
    if (positionRef.current >= timeline.totalDurationSeconds) {
      applyPosition(0, true);
    }
    playingRef.current = true;
    setPlaying(true);
    clockRef.current = {
      startedAtMillis: performance.now(),
      startPositionSeconds: positionRef.current,
    };
    cancelAnimation();
    rafRef.current = requestAnimationFrame(tick);
  }, [timeline, applyPosition, cancelAnimation, tick]);

  const stopPlayback = useCallback(
    (reset: boolean) => {
      pausePlayback();
      if (reset) {
        applyPosition(0, true);
      }
    },
    [pausePlayback, applyPosition],
  );

  const seekToSeconds = useCallback(
    (seconds: number, resumePlayback: boolean) => {
      const shouldResume = resumePlayback && !isTimelineEmpty(timeline);
      pausePlayback();
      applyPosition(seconds, true);
      if (shouldResume && positionRef.current < timeline.totalDurationSeconds) {
        startPlayback();
      }
    },
    [timeline, pausePlayback, applyPosition, startPlayback],
  );

  // Load frames whenever the dialog opens for a replay file.
  useEffect(() => {
    if (!open) {
      return;
    }
    let cancelled = false;
    setLoadState("loading");
    setLoadError(null);
    setFrames([]);
    invoke<TerminalRecordingReplayFrames>("load_terminal_recording_frames", { path: replayPath })
      .then((loaded) => {
        if (cancelled) return;
        setFrames(loaded.frames ?? []);
        setLoadState("ready");
      })
      .catch((error) => {
        if (cancelled) return;
        setFrames([]);
        setLoadError(String(error));
        setLoadState("error");
      });
    return () => {
      cancelled = true;
    };
  }, [open, replayPath]);

  // Reset playback whenever the timeline changes (new frames loaded).
  useEffect(() => {
    playingRef.current = false;
    setPlaying(false);
    cancelAnimation();
    sliderSeekingRef.current = false;
    resumeAfterSliderSeekRef.current = false;
    frameIndexRef.current = -1;
    positionRef.current = 0;
    setTimeJumpText("");
    applyPosition(0, true);
  }, [timeline, applyPosition, cancelAnimation]);

  // Stop the animation loop when the dialog closes or unmounts.
  useEffect(() => {
    if (!open) {
      playingRef.current = false;
      setPlaying(false);
      cancelAnimation();
    }
    return cancelAnimation;
  }, [open, cancelAnimation]);

  const currentFrameContent = useMemo<ReactNode>(() => {
    if (timelineEmpty) {
      return null;
    }
    return renderFrameContent(frameAt(timeline, frameIndex));
  }, [timeline, timelineEmpty, frameIndex]);

  if (!open) return null;

  function togglePlayback() {
    if (playingRef.current) {
      pausePlayback();
    } else {
      startPlayback();
    }
  }

  function handleSpeedChange(rawValue: string) {
    const parsed = Number.parseInt(rawValue, 10);
    const clamped = Number.isFinite(parsed) ? Math.max(1, Math.min(20, parsed)) : 1;
    setSpeed(clamped);
    speedRef.current = clamped;
    // Java: restartPlaybackClockIfPlaying - restart the playback clock at the current position.
    if (playingRef.current) {
      clockRef.current = {
        startedAtMillis: performance.now(),
        startPositionSeconds: positionRef.current,
      };
    }
  }

  function handleSliderPointerDown() {
    if (timelineEmpty) return;
    sliderSeekingRef.current = true;
    resumeAfterSliderSeekRef.current = playingRef.current;
    if (playingRef.current) {
      pausePlayback();
    }
  }

  function handleSliderPointerUp(value: number) {
    if (!sliderSeekingRef.current) return;
    const resumePlayback = resumeAfterSliderSeekRef.current;
    sliderSeekingRef.current = false;
    resumeAfterSliderSeekRef.current = false;
    seekToSeconds(value, resumePlayback);
  }

  function handleSliderChange(value: number) {
    if (timelineEmpty) return;
    if (sliderSeekingRef.current) {
      // Live position update while dragging; playback resumes on pointer up.
      applyPosition(value, false);
    } else {
      // Keyboard arrows / programmatic changes seek directly.
      seekToSeconds(value, playingRef.current);
    }
  }

  function handleTimeJump() {
    if (timelineEmpty) return;
    const seconds = parseTimeJumpSeconds(timeJumpText, timeline.totalDurationSeconds);
    if (seconds == null) {
      setStatusMessage(
        t("recording.viewer.seekInvalid", {
          max: formatRecordingDuration(timeline.totalDurationSeconds),
        }),
      );
      return;
    }
    seekToSeconds(seconds, playingRef.current);
  }

  const frameCount = timeline.frames.length;
  const visibleFrame = timelineEmpty ? 0 : Math.min(frameIndex + 1, frameCount);
  const statusText =
    statusMessage ?? t("recording.viewer.status", { current: visibleFrame, total: frameCount });
  const timeText = t("recording.viewer.time", {
    position: formatRecordingDuration(positionSeconds),
    total: formatRecordingDuration(timeline.totalDurationSeconds),
  });
  const controlsDisabled = timelineEmpty || loadState !== "ready";

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50">
      <div
        className="relative flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex min-w-0 items-center gap-2 text-sm font-semibold">
            <Clapperboard className="h-4 w-4 shrink-0 text-kortty-accent" />
            <span className="shrink-0">{t("recording.viewer.title")}</span>
            <span className="truncate text-xs font-normal text-kortty-text-dim">{replayName}</span>
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} title={t("common.close")}>
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex min-h-0 flex-1 flex-col gap-2 p-3">
          <pre className="min-h-0 flex-1 overflow-auto whitespace-pre rounded border border-kortty-border bg-kortty-terminal p-3 font-mono text-xs leading-[1.25] text-kortty-text">
            {loadState === "loading"
              ? t("recording.viewer.loading")
              : loadState === "error"
                ? t("recording.viewer.loadFailed", { error: loadError ?? "" })
                : timelineEmpty
                  ? t("recording.viewer.empty")
                  : currentFrameContent}
          </pre>

          <div className="flex items-center gap-2 text-xs">
            <span className="shrink-0 text-kortty-text-dim">{t("recording.viewer.timeline")}</span>
            <input
              type="range"
              className="min-w-0 flex-1 accent-kortty-accent"
              min={0}
              max={timelineEmpty ? 1 : timeline.totalDurationSeconds}
              step={0.01}
              value={clampSeconds(timeline, positionSeconds)}
              disabled={controlsDisabled}
              onPointerDown={handleSliderPointerDown}
              onPointerUp={(event) => handleSliderPointerUp(Number(event.currentTarget.value))}
              onChange={(event) => handleSliderChange(Number(event.target.value))}
            />
            <span className="shrink-0 tabular-nums text-kortty-text-dim">{timeText}</span>
          </div>

          <div className="flex flex-wrap items-center gap-2 text-xs">
            <button
              className="btn-secondary flex items-center gap-1.5 text-xs"
              disabled={controlsDisabled}
              onClick={togglePlayback}
            >
              {playing ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
              {playing ? t("recording.viewer.pause") : t("recording.viewer.play")}
            </button>
            <button
              className="btn-secondary flex items-center gap-1.5 text-xs"
              disabled={controlsDisabled}
              onClick={() => stopPlayback(true)}
            >
              <Square className="h-3.5 w-3.5" />
              {t("recording.viewer.stop")}
            </button>
            <span className="ml-2 text-kortty-text-dim">{t("recording.viewer.speed")}</span>
            <input
              type="number"
              className="input-field w-16 text-xs"
              min={1}
              max={20}
              step={1}
              value={speed}
              disabled={controlsDisabled}
              onChange={(event) => handleSpeedChange(event.target.value)}
            />
            <span className="text-kortty-text-dim">{t("recording.viewer.speedSuffix")}</span>
            <span className="ml-3 truncate text-kortty-text-dim">{statusText}</span>
          </div>

          <div className="flex items-center gap-2 text-xs">
            <span className="shrink-0 text-kortty-text-dim">{t("recording.viewer.timeJump")}</span>
            <input
              type="text"
              className="input-field w-24 text-xs"
              placeholder="MM:SS"
              value={timeJumpText}
              disabled={controlsDisabled}
              onChange={(event) => setTimeJumpText(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  handleTimeJump();
                }
              }}
            />
            <button className="btn-secondary text-xs" disabled={controlsDisabled} onClick={handleTimeJump}>
              {t("recording.viewer.seek")}
            </button>
          </div>
        </div>

        <div className="flex justify-end border-t border-kortty-border px-4 py-2">
          <button className="btn-secondary text-xs" onClick={onClose}>
            {t("common.close")}
          </button>
        </div>
        <div
          className="absolute bottom-0 right-0 h-4 w-4 cursor-nwse-resize opacity-40 hover:opacity-100"
          onMouseDown={onResizeStart}
        />
      </div>
    </div>
  );
}
