import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { emitTauriEvent, resetTauriMocks } from "../test-utils/tauri";
import { WAVEFORM_BARS } from "../lib/waveform";
import DictationHud from "./DictationHud";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

/** Listeners are registered in an effect that awaits a promise. */
async function renderHud() {
  const view = render(<DictationHud />);
  await act(async () => {});
  return view;
}

async function setState(state: "idle" | "listening" | "locked" | "processing") {
  await act(async () => {
    emitTauriEvent("dictation:state", { state });
  });
}

async function setLevel(level: number) {
  await act(async () => {
    emitTauriEvent("dictation:level", { level });
  });
}

async function setPartial(
  partial: {
    seq: number;
    text: string;
    stable_chars?: number | null;
    is_final?: boolean;
  },
) {
  await act(async () => {
    emitTauriEvent("dictation:partial", {
      stable_chars: null,
      is_final: false,
      ...partial,
    });
  });
}

const partialText = () =>
  document.querySelector(".kea-hud__partial")?.textContent ?? "";

const realMatchMedia = window.matchMedia;

/** Makes only `(prefers-reduced-motion: reduce)` match. */
function stubReducedMotion() {
  window.matchMedia = ((query: string) =>
    ({
      matches: query.includes("prefers-reduced-motion"),
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList) as typeof window.matchMedia;
}

describe("DictationHud", () => {
  beforeEach(() => resetTauriMocks());
  afterEach(() => {
    window.matchMedia = realMatchMedia;
  });

  it("renders nothing while idle", async () => {
    const { container } = await renderHud();

    expect(container.firstChild).toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("meter")).toBeNull();
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("shows the waveform and the listening state while listening", async () => {
    await renderHud();
    await setState("listening");

    expect(screen.getByRole("status").textContent).toContain("Listening");
    const meter = screen.getByRole("meter", { name: "Microphone level" });
    expect(meter.querySelectorAll(".kea-waveform__bar")).toHaveLength(WAVEFORM_BARS);
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("does not rely on colour alone for the state", async () => {
    await renderHud();
    await setState("listening");

    // A dot carries the state visually, and the label carries it as text.
    expect(document.querySelector(".kea-dot")).not.toBeNull();
    expect(screen.getByRole("status").textContent).toContain("Listening");
  });

  it("scrolls level events through the waveform", async () => {
    await renderHud();
    await setState("listening");
    await setLevel(0.81);

    const meter = screen.getByRole("meter", { name: "Microphone level" });
    await waitFor(() => expect(meter.getAttribute("aria-valuenow")).toBe("0.81"));

    const bars = meter.querySelectorAll<HTMLElement>(".kea-waveform__bar");
    // The newest level lands in the last bar; the ones behind it are still
    // silence, which is what makes the row read as moving.
    expect(bars[bars.length - 1].style.transform).not.toBe(bars[0].style.transform);
  });

  it("shows the indeterminate indicator and its label while processing", async () => {
    await renderHud();
    await setState("processing");

    expect(screen.getByRole("status").textContent).toContain("Transcribing…");
    expect(screen.getByRole("progressbar", { name: "Transcribing" })).toBeTruthy();
    // No levels arrive during transcription, so there is no meter to show.
    expect(screen.queryByRole("meter")).toBeNull();
  });

  it("clears the waveform when a run ends", async () => {
    await renderHud();
    await setState("listening");
    await setLevel(0.9);
    await setState("idle");
    await setState("listening");

    const meter = screen.getByRole("meter", { name: "Microphone level" });
    expect(meter.getAttribute("aria-valuenow")).toBe("0");
  });

  it("shows a locked recording as locked, with both ways out", async () => {
    // A lock has no key holding it open, so the HUD is the only thing saying
    // the microphone is on and how to turn it off.
    await renderHud();
    await setState("locked");

    expect(screen.getByRole("status").textContent).toContain("Locked");
    expect(screen.getByText(/Tap ⌥⇧ to finish · Esc to cancel/)).toBeTruthy();
    // Still recording, so it still shows levels rather than the transcribing
    // shimmer.
    expect(screen.getByRole("meter", { name: "Microphone level" })).toBeTruthy();
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("counts up while locked so a forgotten recording is visible", async () => {
    vi.useFakeTimers();
    try {
      await renderHud();
      await setState("locked");
      expect(screen.getByRole("status").textContent).toContain("0:00");

      await act(async () => {
        vi.advanceTimersByTime(65_000);
      });
      expect(screen.getByRole("status").textContent).toContain("1:05");
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the waveform running across a lock", async () => {
    await renderHud();
    await setState("locked");
    await setLevel(0.7);

    const meter = screen.getByRole("meter", { name: "Microphone level" });
    await waitFor(() => expect(meter.getAttribute("aria-valuenow")).toBe("0.7"));
  });

  it("does not count elapsed time for an ordinary hold", async () => {
    await renderHud();
    await setState("listening");

    expect(screen.getByRole("status").textContent).not.toContain("0:00");
  });

  it("falls back to a static meter when motion is reduced", async () => {
    stubReducedMotion();
    await renderHud();
    await setState("listening");

    expect(screen.getByRole("meter", { name: "Microphone level" })).toBeTruthy();
    expect(document.querySelector(".kea-waveform")).toBeNull();
  });

  /**
   * The most important assertion in the file: this is what a build with no
   * streaming model — the default — actually renders.
   */
  it("shows no transcript line when no partial ever arrives", async () => {
    await renderHud();
    await setState("listening");

    expect(partialText()).toBe("");
    expect(document.querySelector(".kea-hud--has-partial")).toBeNull();
  });

  it("replaces the hypothesis rather than accumulating it", async () => {
    await renderHud();
    await setState("listening");
    await setPartial({ seq: 1, text: "the cat sat on the mat" });
    await setPartial({ seq: 2, text: "the cat sat on the matter" });

    // A streaming recogniser revises its tail; appending deltas would show
    // both.
    expect(partialText()).toBe("the cat sat on the matter");
    expect(partialText().match(/the cat sat on the/g)).toHaveLength(1);
    expect(document.querySelector(".kea-hud--has-partial")).not.toBeNull();
  });

  it("ignores a partial that arrives out of order", async () => {
    await renderHud();
    await setState("listening");
    await setPartial({ seq: 5, text: "newest" });
    await setPartial({ seq: 4, text: "stale" });

    expect(partialText()).toBe("newest");
  });

  it("keeps the transcript across processing and clears it when the run ends", async () => {
    await renderHud();
    await setState("listening");
    await setPartial({ seq: 1, text: "hello wurld" });

    // The window in which the user has stopped talking and wants to see what
    // was heard — and in which the final partial arrives.
    await setState("processing");
    expect(partialText()).toBe("hello wurld");

    await setState("idle");
    await setState("listening");
    expect(partialText()).toBe("");
  });

  it("does not inherit the previous run's words", async () => {
    await renderHud();
    await setState("listening");
    await setPartial({ seq: 1, text: "first run" });
    await setState("processing");
    await setState("listening");

    expect(partialText()).toBe("");
  });

  it("splits settled text from the tail the engine is still revising", async () => {
    await renderHud();
    await setState("listening");
    await setPartial({ seq: 1, text: "hello wurld", stable_chars: 6 });

    expect(document.querySelector(".kea-hud__partial-stable")?.textContent).toBe("hello ");
    expect(document.querySelector(".kea-hud__partial-tail")?.textContent).toBe("wurld");
  });

  it("treats an engine that reports no stability as all settled", async () => {
    await renderHud();
    await setState("listening");
    await setPartial({ seq: 1, text: "hello wurld", stable_chars: null });

    expect(document.querySelector(".kea-hud__partial-stable")?.textContent).toBe("hello wurld");
    expect(document.querySelector(".kea-hud__partial-tail")?.textContent).toBe("");
  });

  /**
   * `stable_chars` counts Unicode scalar values. A byte or UTF-16 offset would
   * cut these in half and render a replacement character — invisible in
   * English-only testing, which is exactly why it is pinned here.
   */
  it("splits on a scalar boundary, not a byte or surrogate one", async () => {
    await renderHud();
    await setState("listening");
    // Two astral emoji and a CJK character: 3 scalars, 11 UTF-8 bytes,
    // 5 UTF-16 code units.
    await setPartial({ seq: 1, text: "🎤🐈猫ok", stable_chars: 3 });

    const stable = document.querySelector(".kea-hud__partial-stable")?.textContent ?? "";
    const tail = document.querySelector(".kea-hud__partial-tail")?.textContent ?? "";
    expect(stable).toBe("🎤🐈猫");
    expect(tail).toBe("ok");
    expect(stable + tail).not.toContain("\uFFFD");
  });

  /**
   * A live region fed a self-revising string ten times a second is unusable,
   * so only the state label is announced.
   */
  it("keeps the transcript out of the live region", async () => {
    await renderHud();
    await setState("listening");
    await setPartial({ seq: 1, text: "hello wurld" });

    expect(screen.getByRole("status").textContent).toContain("Listening");
    expect(screen.getByRole("status").textContent).not.toContain("wurld");
    expect(document.querySelector(".kea-hud__partial")?.getAttribute("aria-hidden")).toBe(
      "true",
    );
  });

  it("still falls back to a static meter with a partial on screen", async () => {
    stubReducedMotion();
    await renderHud();
    await setState("listening");
    await setPartial({ seq: 1, text: "hello wurld" });

    expect(screen.getByRole("meter", { name: "Microphone level" })).toBeTruthy();
    expect(document.querySelector(".kea-waveform")).toBeNull();
    expect(partialText()).toBe("hello wurld");
  });

  it("stills the transcribing indicator when motion is reduced", async () => {
    stubReducedMotion();
    await renderHud();
    await setState("processing");

    const indicator = screen.getByRole("progressbar", { name: "Transcribing" });
    expect(indicator.className).toContain("kea-shimmer--static");
  });
});
