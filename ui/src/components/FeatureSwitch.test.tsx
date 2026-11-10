import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import FeatureSwitch, {
  FEATURE_SUMMARIES,
  FeaturePageHeader,
  FeatureSwitchOverview,
} from "./FeatureSwitch";
import { emitTauriEvent, invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);
beforeEach(resetTauriMocks);

/** A feature page as the real ones are built: the shared header, then its controls. */
function Page() {
  return (
    <FeatureSwitch feature="dictation">
      <div>
        <FeaturePageHeader />
        <button>Record</button>
      </div>
    </FeatureSwitch>
  );
}

const follows = (a: Element, b: Element) =>
  Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);

describe("feature master switches", () => {
  it("puts the switch under the page title, described by what the feature does", async () => {
    render(<Page />);
    const title = await screen.findByRole("heading", { level: 1, name: "Dictation" });
    const toggle = screen.getByRole("switch", { name: "Enable Dictation" });
    expect(follows(title, toggle)).toBe(true);
    expect(follows(toggle, screen.getByText("Record"))).toBe(true);
    const hint = document.getElementById(toggle.getAttribute("aria-describedby") ?? "");
    expect(hint?.textContent).toBe(FEATURE_SUMMARIES.dictation);
  });
  it("keeps a disabled page's title and switch and hides its controls, then enables live", async () => {
    onInvoke({ get_feature_enabled: () => false, set_feature_enabled: () => undefined });
    render(<Page />);
    await waitFor(() => expect(screen.queryByText("Record")).toBeNull());
    expect(screen.getByText(/^Dictation is turned off\./)).toBeTruthy();
    const title = screen.getByRole("heading", { level: 1, name: "Dictation" });
    const toggle = screen.getByRole("switch", { name: "Enable Dictation" });
    expect(follows(title, toggle)).toBe(true);
    await userEvent.click(toggle);
    expect(await screen.findByText("Record")).toBeTruthy();
    expect(invokeCalls("set_feature_enabled")).toEqual([{ feature: "dictation", enabled: true }]);
  });
  it("lists all six features in one group and follows backend changes", async () => {
    let enabled = true;
    onInvoke({ get_feature_enabled: () => enabled });
    render(<FeatureSwitchOverview />);
    expect(screen.getAllByRole("switch")).toHaveLength(6);
    expect(screen.getAllByRole("group")).toHaveLength(1);
    enabled = false;
    await act(async () => { emitTauriEvent("feature-switch-changed", "rewrite"); });
    expect(screen.getByRole("switch", { name: "Enable Rewrite" }).getAttribute("aria-checked")).toBe("false");
    expect(screen.getByRole("switch", { name: "Enable Dictation" }).getAttribute("aria-checked")).toBe("true");
  });
  it("shows failed saves without claiming the switch changed", async () => {
    onInvoke({ set_feature_enabled: () => { throw new Error("database unavailable"); } });
    render(<FeatureSwitch feature="rewrite"><FeaturePageHeader /></FeatureSwitch>);
    await userEvent.click(screen.getByRole("switch"));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Error: database unavailable");
    expect(screen.getByRole("switch").getAttribute("aria-checked")).toBe("true");
  });
  it("reports a failed save in the overview on the feature's own row", async () => {
    onInvoke({ set_feature_enabled: () => { throw new Error("database unavailable"); } });
    render(<FeatureSwitchOverview />);
    const toggle = screen.getByRole("switch", { name: "Enable Rewrite" });
    await userEvent.click(toggle);
    await waitFor(() => {
      const hint = document.getElementById(toggle.getAttribute("aria-describedby") ?? "");
      expect(hint?.textContent).toBe("Error: database unavailable");
    });
    expect(toggle.getAttribute("aria-checked")).toBe("true");
  });
});
