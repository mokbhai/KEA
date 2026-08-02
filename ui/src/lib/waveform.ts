/** Number of bars the dictation waveform shows. */
export const WAVEFORM_BARS = 28;

/** Shortest bar, as a fraction of the track height, so silence still reads as a row of bars. */
export const WAVEFORM_MIN_SCALE = 0.08;

/** A history of levels, oldest first, always exactly `size` long. */
export function emptyLevelHistory(size = WAVEFORM_BARS): number[] {
  return new Array<number>(size).fill(0);
}

/**
 * Appends a level and drops the oldest, so rendering the array left-to-right
 * scrolls. Levels arrive every 50ms, one bar each — a fixed-length window is
 * what makes the row move rather than grow.
 */
export function pushLevel(
  history: readonly number[],
  level: number,
  size = WAVEFORM_BARS,
): number[] {
  const clamped = Math.max(0, Math.min(1, Number.isFinite(level) ? level : 0));
  return [...history, clamped].slice(-size);
}

/**
 * Vertical scale for a bar. RMS levels from a normal speaking voice sit near
 * the bottom of 0..1, so a linear bar barely moves; the square root opens the
 * quiet end out, and the floor keeps a bar visible at silence.
 */
export function barScale(level: number): number {
  const clamped = Math.max(0, Math.min(1, Number.isFinite(level) ? level : 0));
  return WAVEFORM_MIN_SCALE + (1 - WAVEFORM_MIN_SCALE) * Math.sqrt(clamped);
}
