import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import { ThemeProvider } from "../theme";
import GeneralPage from "./GeneralPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

function mockGeneralWorld() {
  const settings = new Map<string, string>();
  onInvoke({
    get_setting: (args) => settings.get(args?.key as string) ?? null,
    set_setting: (args) => {
      settings.set(args?.key as string, args?.value as string);
    },
    get_autostart: () => false,
    set_autostart: () => undefined,
    get_all_permission_statuses: () => [],
  });
}

function renderPage() {
  return render(
    <ThemeProvider>
      <GeneralPage onRunSetup={() => {}} />
    </ThemeProvider>,
  );
}

describe("GeneralPage", () => {
  beforeEach(() => resetTauriMocks());

  it("saves the appearance preference from the row", async () => {
    mockGeneralWorld();
    renderPage();

    const select = await screen.findByRole("combobox", { name: "Appearance" });
    await userEvent.selectOptions(select, "dark");

    expect(document.documentElement.dataset.theme).toBe("dark");
    await waitFor(() =>
      expect(
        invokeCalls("set_setting").some(
          (args) => args?.key === "ui.theme" && args?.value === "dark",
        ),
      ).toBe(true),
    );
    expect(screen.getByText("Saved ✓")).toBeTruthy();
  });

  it("toggles launch at login and persists it", async () => {
    mockGeneralWorld();
    renderPage();

    const toggle = await screen.findByRole("switch", { name: "Launch KEA at login" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");

    await userEvent.click(toggle);

    await waitFor(() => expect(invokeCalls("set_autostart")).toHaveLength(1));
    expect(invokeCalls("set_autostart")[0]).toEqual({ enabled: true });
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    expect(screen.getByText("Saved ✓")).toBeTruthy();
  });
});
