import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { onInvoke, resetTauriMocks } from "./test-utils/tauri";
import { featureHandlers, openAiBinding } from "./test-utils/featureWorld";
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

  it("puts the theme toggle and version in their own landmark, not the nav", async () => {
    await renderShell();

    const nav = screen.getByRole("navigation", { name: "Main navigation" });
    expect(
      within(nav).queryByRole("button", { name: /Switch to (light|dark) mode/ }),
    ).toBeNull();
    expect(within(nav).queryByText("v0.1.0")).toBeNull();

    // Not merely outside <nav> — inside a region of its own, so it is not
    // orphaned outside every landmark.
    const footer = screen.getByRole("region", { name: "Theme and version" });
    expect(
      within(footer).getByRole("button", { name: /Switch to (light|dark) mode/ }),
    ).toBeTruthy();
    await waitFor(() => expect(within(footer).getByText("v0.1.0")).toBeTruthy());
    // Still inside the same sidebar column, so nothing moved visually.
    expect(drawer().contains(footer)).toBe(true);
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

  it("shuts the page behind the open drawer out too", async () => {
    narrow = true;
    await renderShell();

    const main = document.querySelector("main")!;
    expect(main.hasAttribute("inert")).toBe(false);

    await userEvent.click(screen.getByRole("button", { name: "Open menu" }));
    // Tab is trapped, but a virtual cursor or a pointer would otherwise still
    // reach the page underneath.
    await waitFor(() => expect(main.hasAttribute("inert")).toBe(true));

    await userEvent.keyboard("{Escape}");
    await waitFor(() => expect(main.hasAttribute("inert")).toBe(false));
  });

  it("leaves focus alone when navigating with the drawer closed", async () => {
    // A feature banner's fix action calls the same navigate() the nav items
    // do. On mobile that closes the (already closed) drawer, which must not
    // drag focus up to the topbar.
    onInvoke(
      featureHandlers({
        engines: { llm: ["openai"] },
        hasKey: false,
        bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
        extra: {
          get_setting: (args) => (args?.key === "onboarding.completed" ? "true" : null),
          set_setting: () => undefined,
          list_presets: () => [],
          get_prompt_override: () => null,
          list_whisper_models: () => [],
          list_installed_whisper_models: () => [],
        },
      }),
    );
    narrow = true;
    await renderShell();

    const menuButton = screen.getByRole("button", { name: "Open menu" });
    const fix = await screen.findByRole("button", { name: "Open AI Providers" });
    await userEvent.click(fix);

    expect(
      await screen.findByRole("heading", { level: 1, name: "AI Providers" }),
    ).toBeTruthy();
    expect(document.activeElement).not.toBe(menuButton);
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
