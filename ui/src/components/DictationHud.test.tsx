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

async function setState(state: "idle" | "listening" | "processing") {
  await act(async () => {
    emitTauriEvent("dictation:state", { state });
  });
}

async function setLevel(level: number) {
  await act(async () => {
    emitTauriEvent("dictation:level", { level });
  });
}

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

  it("falls back to a static meter when motion is reduced", async () => {
    stubReducedMotion();
    await renderHud();
    await setState("listening");

    expect(screen.getByRole("meter", { name: "Microphone level" })).toBeTruthy();
    expect(document.querySelector(".kea-waveform")).toBeNull();
  });

  it("stills the transcribing indicator when motion is reduced", async () => {
    stubReducedMotion();
    await renderHud();
    await setState("processing");

    const indicator = screen.getByRole("progressbar", { name: "Transcribing" });
    expect(indicator.className).toContain("kea-shimmer--static");
  });
});
