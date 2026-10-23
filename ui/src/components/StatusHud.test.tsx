import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { emitTauriEvent, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import StatusHud from "./StatusHud";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

describe("StatusHud", () => {
  let overlayVisibleCalls: boolean[] = [];

  beforeEach(() => {
    resetTauriMocks();
    vi.useFakeTimers();
    overlayVisibleCalls = [];
    onInvoke({
      set_status_overlay_visible: (args) => {
        const visible = (args as { visible: boolean }).visible;
        overlayVisibleCalls.push(visible);
        return null;
      },
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  async function renderHud() {
    const view = render(<StatusHud />);
    await act(async () => {});
    return view;
  }

  const getHud = () => document.querySelector(".kea-hud--status");

  it("renders nothing when there is no active status", async () => {
    await renderHud();
    expect(getHud()).toBeNull();
  });

  it("shows in-flight rewrite progress and calls set_status_overlay_visible(true)", async () => {
    await renderHud();

    await act(async () => {
      emitTauriEvent("rewrite:progress", { message: "Rewriting selection..." });
    });

    const hud = getHud();
    expect(hud).not.toBeNull();
    expect(hud?.textContent).toContain("Rewriting selection...");
    expect(hud?.className).toContain("kea-status-pill--progress");
    expect(overlayVisibleCalls).toContain(true);
  });

  it("shows Done on completion and auto-dismisses after 3 seconds", async () => {
    await renderHud();

    await act(async () => {
      emitTauriEvent("rewrite:progress", { message: "Done" });
    });

    expect(getHud()?.textContent).toContain("Done");

    // Advance 2.9 seconds -> still present
    act(() => {
      vi.advanceTimersByTime(2900);
    });
    expect(getHud()?.textContent).toContain("Done");

    // Advance past 3.0 seconds -> dismissed and calls set_status_overlay_visible(false)
    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(getHud()).toBeNull();
    expect(overlayVisibleCalls).toContain(false);
  });

  it("shows rewrite error with error styling and auto-dismisses after 4 seconds", async () => {
    await renderHud();

    await act(async () => {
      emitTauriEvent("rewrite:error", { message: "API key is missing" });
    });

    const hud = getHud();
    expect(hud).not.toBeNull();
    expect(hud?.textContent).toContain("API key is missing");
    expect(hud?.className).toContain("kea-status-pill--error");

    act(() => {
      vi.advanceTimersByTime(3900);
    });
    expect(getHud()?.textContent).toContain("API key is missing");

    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(getHud()).toBeNull();
    expect(overlayVisibleCalls).toContain(false);
  });

  it("handles dictation errors", async () => {
    await renderHud();

    await act(async () => {
      emitTauriEvent("dictation:error", { message: "Microphone permission denied" });
    });

    const hud = getHud();
    expect(hud).not.toBeNull();
    expect(hud?.textContent).toContain("Microphone permission denied");
    expect(hud?.className).toContain("kea-status-pill--error");
  });

  it("handles TTS reading and finished states", async () => {
    await renderHud();

    await act(async () => {
      emitTauriEvent("tts:state", { state: "reading" });
    });
    expect(getHud()?.textContent).toContain("Reading selection aloud…");

    await act(async () => {
      emitTauriEvent("tts:state", { state: "idle" });
    });
    expect(getHud()?.textContent).toContain("Done");

    act(() => {
      vi.advanceTimersByTime(3100);
    });
    expect(getHud()).toBeNull();
  });

  it("handles TTS error", async () => {
    await renderHud();

    await act(async () => {
      emitTauriEvent("tts:error", { message: "Synthesis failed" });
    });

    const hud = getHud();
    expect(hud).not.toBeNull();
    expect(hud?.textContent).toContain("Synthesis failed");
    expect(hud?.className).toContain("kea-status-pill--error");
  });

  it("handles meeting errors", async () => {
    await renderHud();

    await act(async () => {
      emitTauriEvent("meeting:error", { message: "Audio device disconnected" });
    });

    const hud = getHud();
    expect(hud).not.toBeNull();
    expect(hud?.textContent).toContain("Audio device disconnected");
    expect(hud?.className).toContain("kea-status-pill--error");
  });

  it("handles meeting voice separation unavailable", async () => {
    await renderHud();

    await act(async () => {
      emitTauriEvent("meeting:voice_separation_unavailable", {
        reason: "Model not downloaded",
      });
    });

    const hud = getHud();
    expect(hud).not.toBeNull();
    expect(hud?.textContent).toContain(
      "Voice separation is off for this meeting: Model not downloaded",
    );
  });

  it("cleans up active timers on unmount", async () => {
    const { unmount } = await renderHud();

    await act(async () => {
      emitTauriEvent("rewrite:progress", { message: "Done" });
    });

    unmount();

    // Advance time after unmount should not throw or error
    act(() => {
      vi.advanceTimersByTime(5000);
    });
  });
});
