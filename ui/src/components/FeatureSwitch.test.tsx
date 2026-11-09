import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import FeatureSwitch, { FeatureSwitchOverview } from "./FeatureSwitch";
import { emitTauriEvent, invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);
beforeEach(resetTauriMocks);
describe("feature master switches", () => {
  it("keeps a disabled page's switch and hides its controls, then enables live", async () => {
    onInvoke({ get_feature_enabled: () => false, set_feature_enabled: () => undefined });
    render(<FeatureSwitch feature="dictation"><button>Record</button></FeatureSwitch>);
    await waitFor(() => expect(screen.queryByText("Record")).toBeNull());
    expect(screen.getByText("Turned off")).toBeTruthy();
    await userEvent.click(screen.getByRole("switch", { name: "Enable Dictation" }));
    expect(await screen.findByText("Record")).toBeTruthy();
    expect(invokeCalls("set_feature_enabled")).toEqual([{ feature: "dictation", enabled: true }]);
  });
  it("lists all six features and follows backend changes", async () => {
    let enabled = true;
    onInvoke({ get_feature_enabled: () => enabled });
    render(<FeatureSwitchOverview />);
    expect(screen.getAllByRole("switch")).toHaveLength(6);
    enabled = false;
    await act(async () => { emitTauriEvent("feature-switch-changed", "rewrite"); });
    expect(screen.getByRole("switch", { name: "Enable Rewrite" }).getAttribute("aria-checked")).toBe("false");
    expect(screen.getByRole("switch", { name: "Enable Dictation" }).getAttribute("aria-checked")).toBe("true");
  });
  it("shows failed saves without claiming the switch changed", async () => {
    onInvoke({ set_feature_enabled: () => { throw new Error("database unavailable"); } });
    render(<FeatureSwitch feature="rewrite" />);
    await userEvent.click(screen.getByRole("switch"));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Error: database unavailable");
    expect(screen.getByRole("switch").getAttribute("aria-checked")).toBe("true");
  });
});
