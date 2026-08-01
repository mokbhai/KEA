import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { onInvoke, resetTauriMocks } from "./test-utils/tauri";
import { featureHandlers } from "./test-utils/featureWorld";
import App from "./App";

vi.mock("@tauri-apps/api/core", async () => (await import("./test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("./test-utils/tauri")).eventModule);
vi.mock("@tauri-apps/api/app", () => ({ getVersion: () => Promise.resolve("0.1.0") }));

const realMatchMedia = window.matchMedia;
const realInnerWidth = window.innerWidth;

let narrow = false;

/**
 * matchMedia that answers the shell's width query from `narrow` and every
 * other query (prefers-color-scheme) with false.
 */
function installMatchMedia() {
  window.matchMedia = ((query: string) =>
    ({
      media: query,
      get matches() {
        return query.includes("max-width") ? narrow : false;
      },
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList) as typeof window.matchMedia;
}

function mockShellWorld() {
  onInvoke(
    featureHandlers({
      engines: { llm: ["openai"] },
      extra: {
        get_setting: (args) =>
          args?.key === "onboarding.completed" ? "true" : null,
        set_setting: () => undefined,
        list_presets: () => [],
        get_prompt_override: () => null,
      },
    }),
  );
}

/** Waits past the onboarding check so the shell (not the spinner) is on screen. */
async function renderShell() {
  render(<App />);
  await screen.findByRole("navigation", { name: "Main navigation" });
}

const drawer = () => document.getElementById("kea-nav")!;

describe("AppShell", () => {
  beforeEach(() => {
    resetTauriMocks();
    mockShellWorld();
    narrow = false;
    installMatchMedia();
  });

  afterEach(() => {
    window.matchMedia = realMatchMedia;
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: realInnerWidth,
    });
  });

  it("keeps the theme toggle and version out of the navigation landmark", async () => {
    await renderShell();

    const nav = screen.getByRole("navigation", { name: "Main navigation" });
    expect(
      within(nav).queryByRole("button", { name: /Switch to (light|dark) mode/ }),
    ).toBeNull();
    expect(
      screen.getByRole("button", { name: /Switch to (light|dark) mode/ }),
    ).toBeTruthy();
    await waitFor(() => expect(screen.getByText("v0.1.0")).toBeTruthy());
    expect(within(nav).queryByText("v0.1.0")).toBeNull();
    // Still inside the same sidebar column, so nothing moved visually.
    expect(drawer().contains(screen.getByText("v0.1.0"))).toBe(true);
  });

  it("takes the mobile layout from matchMedia, not window.innerWidth", async () => {
    // A wide innerWidth with a matching narrow media query: the old dual
    // source seeded desktop here and only corrected on the next resize.
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1440 });
    narrow = true;
    await renderShell();

    expect(screen.getByRole("button", { name: "Open menu" })).toBeTruthy();
  });

  it("leaves the desktop sidebar reachable", async () => {
    await renderShell();

    expect(drawer().hasAttribute("inert")).toBe(false);
    expect(screen.queryByRole("button", { name: "Open menu" })).toBeNull();
  });

  it("keeps the closed mobile drawer out of the keyboard path", async () => {
    narrow = true;
    await renderShell();

    expect(drawer().hasAttribute("inert")).toBe(true);

    await userEvent.click(screen.getByRole("button", { name: "Open menu" }));
    await waitFor(() => expect(drawer().hasAttribute("inert")).toBe(false));
  });

  it("closes the drawer on Escape and gives focus back to the menu button", async () => {
    narrow = true;
    await renderShell();

    const menuButton = screen.getByRole("button", { name: "Open menu" });
    await userEvent.click(menuButton);

    // Opening moves focus into the drawer.
    await waitFor(() =>
      expect(drawer().contains(document.activeElement)).toBe(true),
    );
    expect(menuButton.getAttribute("aria-expanded")).toBe("true");

    await userEvent.keyboard("{Escape}");

    await waitFor(() => expect(menuButton.getAttribute("aria-expanded")).toBe("false"));
    expect(drawer().hasAttribute("inert")).toBe(true);
    expect(document.activeElement).toBe(menuButton);
  });

  it("wraps Tab around inside the open drawer", async () => {
    narrow = true;
    await renderShell();

    await userEvent.click(screen.getByRole("button", { name: "Open menu" }));
    await waitFor(() =>
      expect(drawer().contains(document.activeElement)).toBe(true),
    );

    const focusable = Array.from(
      drawer().querySelectorAll<HTMLElement>("button:not([disabled])"),
    );
    const first = focusable[0];
    const last = focusable[focusable.length - 1];

    last.focus();
    await userEvent.keyboard("{Tab}");
    expect(document.activeElement).toBe(first);

    await userEvent.keyboard("{Shift>}{Tab}{/Shift}");
    expect(document.activeElement).toBe(last);
  });

  it("closes the drawer after navigating from it", async () => {
    narrow = true;
    await renderShell();

    const menuButton = screen.getByRole("button", { name: "Open menu" });
    await userEvent.click(menuButton);
    await userEvent.click(await screen.findByRole("button", { name: /Logs/ }));

    expect(await screen.findByRole("heading", { level: 1, name: "Logs" })).toBeTruthy();
    await waitFor(() => expect(drawer().hasAttribute("inert")).toBe(true));
    expect(document.activeElement).toBe(menuButton);
  });
});
