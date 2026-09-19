import { act } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { onInvoke, resetTauriMocks } from "../test-utils/tauri";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ label: "palette" }),
}));

/**
 * The window-label branch in `main.tsx`, which is the only thing that makes
 * one bundle serve three windows. A palette window that fell through to `App`
 * would render the whole settings shell into a 640x300 borderless box.
 */
describe("the palette window branch", () => {
  beforeEach(() => {
    resetTauriMocks();
    vi.resetModules();
    document.body.innerHTML = '<div id="root"></div>';
    delete document.documentElement.dataset.window;
    document.documentElement.style.background = "";
    document.body.style.background = "";
  });

  it("renders the palette, not the settings shell, and goes transparent", async () => {
    onInvoke({});
    await act(async () => {
      await import("../main");
    });

    expect(document.documentElement.dataset.window).toBe("palette");
    // The window is transparent; the stylesheet's opaque body would fill it
    // back in, and the flash is visible on every open.
    expect(document.documentElement.style.background).toBe("transparent");
    expect(document.body.style.background).toBe("transparent");
    // No session yet, so the palette renders nothing at all — and in
    // particular none of `App`'s navigation.
    expect(document.querySelector("nav")).toBeNull();
    expect(document.getElementById("root")!.textContent).toBe("");
  });
});
