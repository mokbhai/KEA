import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import PaletteSettings from "./PaletteSettings";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

function mockSettings(stored: Record<string, string | null> = {}) {
  onInvoke({
    get_setting: (args) => stored[(args as { key: string }).key] ?? null,
    set_setting: () => undefined,
    get_effective_hotkey: () => null,
    get_hotkey_registration_status: () => [],
    get_ocr_languages: () => ["en-US", "ja"],
    clear_palette_history: () => undefined,
    open_palette: () => undefined,
    capture_screen_text: () => undefined,
  });
}

async function renderSettings(stored: Record<string, string | null> = {}) {
  mockSettings(stored);
  const view = render(<PaletteSettings />);
  await act(async () => {});
  return view;
}

const toggle = (label: string) => screen.getByRole("switch", { name: label });

describe("PaletteSettings", () => {
  beforeEach(() => {
    resetTauriMocks();
  });

  it("defaults to on when no row has been written", async () => {
    await renderSettings();
    expect(toggle("Check the selection before replacing it").getAttribute("aria-checked")).toBe(
      "true",
    );
    expect(toggle("Remember palette instructions").getAttribute("aria-checked")).toBe("true");
    expect(toggle("Correct spelling in captured text").getAttribute("aria-checked")).toBe(
      "true",
    );
  });

  it("reads back the string encoding the generic set_setting writes", async () => {
    // A bool written by `set_setting` arrives as the JSON *string* "false". A
    // reader that expected a JSON bool here would fall back to its default and
    // the toggle would be silently inert — which is exactly what happened to
    // two other flags in this app.
    await renderSettings({
      "palette.verify_selection": "false",
      "ocr.language_correction": "false",
    });
    expect(toggle("Check the selection before replacing it").getAttribute("aria-checked")).toBe(
      "false",
    );
    expect(toggle("Correct spelling in captured text").getAttribute("aria-checked")).toBe(
      "false",
    );
  });

  it("writes a flipped toggle as the same string encoding", async () => {
    await renderSettings();
    await userEvent.click(toggle("Check the selection before replacing it"));
    expect(invokeCalls("set_setting")).toContainEqual({
      key: "palette.verify_selection",
      value: "false",
    });
  });

  it("puts the real supported-language list in the hint", async () => {
    // Shipped as a guess it would be wrong on some macOS version; Vision is
    // asked instead.
    await renderSettings();
    expect(screen.getByText(/This Mac supports: en-US, ja\./)).toBeTruthy();
  });

  it("saves the language list on blur, not per keystroke", async () => {
    await renderSettings();
    const field = screen.getByLabelText("Capture languages");
    await userEvent.type(field, "ja");
    expect(invokeCalls("set_setting")).toEqual([]);
    await userEvent.tab();
    expect(invokeCalls("set_setting")).toEqual([
      { key: "ocr.languages", value: "ja" },
    ]);
  });

  it("clears the instruction history and says so", async () => {
    await renderSettings();
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(invokeCalls("clear_palette_history").length).toBe(1);
    expect(screen.getByText("Instruction history cleared.")).toBeTruthy();
  });

  it("offers the shortcut-free way in for both commands", async () => {
    // The case the shortcut cannot cover: another app already owns the combo
    // and the OS refused to register it.
    await renderSettings();
    await userEvent.click(screen.getByRole("button", { name: "Open the palette" }));
    await userEvent.click(screen.getByRole("button", { name: "Capture screen text" }));
    expect(invokeCalls("open_palette").length).toBe(1);
    expect(invokeCalls("capture_screen_text").length).toBe(1);
  });

  it("reports a refused write and puts the toggle back", async () => {
    onInvoke({
      get_setting: () => null,
      get_effective_hotkey: () => null,
      get_hotkey_registration_status: () => [],
      get_ocr_languages: () => [],
      set_setting: () => {
        throw new Error("database is locked");
      },
    });
    render(<PaletteSettings />);
    await act(async () => {});

    await userEvent.click(toggle("Remember palette instructions"));
    expect(screen.getByRole("alert").textContent).toContain("database is locked");
    expect(toggle("Remember palette instructions").getAttribute("aria-checked")).toBe("true");
  });
});
