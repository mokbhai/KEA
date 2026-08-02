import { describe, expect, it } from "vitest";
import {
  barScale,
  emptyLevelHistory,
  pushLevel,
  WAVEFORM_BARS,
  WAVEFORM_MIN_SCALE,
} from "./waveform";

describe("emptyLevelHistory", () => {
  it("is a full-length row of zeroes", () => {
    expect(emptyLevelHistory()).toHaveLength(WAVEFORM_BARS);
    expect(emptyLevelHistory().every((v) => v === 0)).toBe(true);
  });

  it("honours an explicit size", () => {
    expect(emptyLevelHistory(4)).toEqual([0, 0, 0, 0]);
  });
});

describe("pushLevel", () => {
  it("appends to the end and keeps the length fixed", () => {
    const next = pushLevel([0, 0, 0, 0], 0.5, 4);
    expect(next).toEqual([0, 0, 0, 0.5]);
  });

  it("drops the oldest sample so the row scrolls", () => {
    let history = [0.1, 0.2, 0.3, 0.4];
    history = pushLevel(history, 0.5, 4);
    expect(history).toEqual([0.2, 0.3, 0.4, 0.5]);
  });

  it("does not mutate the history it was given", () => {
    const history = [0.1, 0.2];
    pushLevel(history, 0.9, 2);
    expect(history).toEqual([0.1, 0.2]);
  });

  it("clamps out-of-range and non-finite levels", () => {
    expect(pushLevel([0], 5, 1)).toEqual([1]);
    expect(pushLevel([0], -5, 1)).toEqual([0]);
    expect(pushLevel([0], Number.NaN, 1)).toEqual([0]);
  });

  it("fills a short history up to the window size", () => {
    expect(pushLevel([], 0.5, 3)).toEqual([0.5]);
  });
});

describe("barScale", () => {
  it("keeps a visible bar at silence", () => {
    expect(barScale(0)).toBe(WAVEFORM_MIN_SCALE);
  });

  it("reaches full height at the top of the range", () => {
    expect(barScale(1)).toBeCloseTo(1);
  });

  it("is monotonic and never leaves 0..1", () => {
    const scales = [0, 0.1, 0.25, 0.5, 0.75, 1].map(barScale);
    for (let i = 1; i < scales.length; i += 1) {
      expect(scales[i]).toBeGreaterThan(scales[i - 1]);
    }
    expect(scales.every((s) => s >= 0 && s <= 1)).toBe(true);
  });

  it("opens out the quiet end a linear bar would flatten", () => {
    // A 0.1 RMS is a normal speaking level; linearly it would be a 10% bar.
    expect(barScale(0.1)).toBeGreaterThan(0.25);
  });

  it("clamps out-of-range and non-finite levels", () => {
    expect(barScale(9)).toBeCloseTo(1);
    expect(barScale(-9)).toBe(WAVEFORM_MIN_SCALE);
    expect(barScale(Number.NaN)).toBe(WAVEFORM_MIN_SCALE);
  });
});
