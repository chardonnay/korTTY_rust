/**
 * Parses replay time-jump input (port of TerminalRecordingTimeJumpParser.java).
 * Accepts either "MM:SS" or decimal minutes with comma or dot ("1,5" = 90s).
 * Returns the parsed position in seconds, or null when the input is invalid or
 * outside the replay duration.
 */

const SECONDS_PER_MINUTE = 60;
const INTEGER_PATTERN = /^[+-]?\d+$/;
const DECIMAL_PATTERN = /^[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?$/;

export function parseTimeJumpSeconds(
  text: string | null | undefined,
  maxSeconds: number,
): number | null {
  if (!text || !text.trim() || !Number.isFinite(maxSeconds) || maxSeconds < 0) {
    return null;
  }

  const parsedSeconds = text.includes(":") ? parseMinutesAndSeconds(text) : parseMinutes(text);
  if (parsedSeconds == null) {
    return null;
  }

  if (parsedSeconds < 0 || parsedSeconds > maxSeconds + 0.0001) {
    return null;
  }
  return parsedSeconds;
}

function parseMinutes(text: string): number | null {
  const normalized = text.trim().replace(/,/g, ".");
  if (!DECIMAL_PATTERN.test(normalized)) {
    return null;
  }
  const minutes = Number(normalized);
  return Number.isFinite(minutes) ? minutes * SECONDS_PER_MINUTE : null;
}

function parseMinutesAndSeconds(text: string): number | null {
  const parts = text.trim().split(":");
  if (parts.length !== 2 || !parts[0].trim() || !parts[1].trim()) {
    return null;
  }
  const minutesText = parts[0].trim();
  const secondsText = parts[1].trim();
  if (!INTEGER_PATTERN.test(minutesText) || !INTEGER_PATTERN.test(secondsText)) {
    return null;
  }
  const minutes = Number.parseInt(minutesText, 10);
  const seconds = Number.parseInt(secondsText, 10);
  if (minutes < 0 || seconds < 0 || seconds >= 60) {
    return null;
  }
  return minutes * SECONDS_PER_MINUTE + seconds;
}
