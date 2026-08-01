import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Row, RowGroup } from "./SettingsRow";
import Toggle from "./Toggle";

describe("RowGroup + Row", () => {
  it("renders label, hint, and control", () => {
    render(
      <RowGroup aria-label="Dictation settings">
        <Row label="Hotkey" hint="hold to talk">
          <button>⌘⇧D</button>
        </Row>
      </RowGroup>,
    );
    expect(screen.getByRole("group", { name: "Dictation settings" })).toBeTruthy();
    expect(screen.getByText("Hotkey")).toBeTruthy();
    expect(screen.getByText("hold to talk")).toBeTruthy();
    expect(screen.getByRole("button", { name: "⌘⇧D" })).toBeTruthy();
  });

  it("renders without a hint", () => {
    render(
      <RowGroup>
        <Row label="Enable Dictation" />
      </RowGroup>,
    );
    expect(screen.getByText("Enable Dictation")).toBeTruthy();
    expect(document.querySelector(".kea-row__hint")).toBeNull();
  });

  it("describes a Toggle control with the row hint", () => {
    render(
      <RowGroup>
        <Row label="Launch at login" hint="Start KEA when you log in">
          <Toggle checked={false} onChange={() => {}} label="Launch KEA at login" />
        </Row>
      </RowGroup>,
    );
    const toggle = screen.getByRole("switch", { name: "Launch KEA at login" });
    const hintId = toggle.getAttribute("aria-describedby");
    expect(hintId).toBeTruthy();
    expect(document.getElementById(hintId!)?.textContent).toBe(
      "Start KEA when you log in",
    );
  });

  it("describes native select and input controls with the row hint", () => {
    render(
      <RowGroup>
        <Row label="Appearance" hint="Follow this Mac or pick a theme">
          <select aria-label="Appearance">
            <option>System</option>
          </select>
        </Row>
        <Row label="Transcribe every" hint="How often segments appear">
          <input aria-label="Seconds per segment" type="number" defaultValue={15} />
        </Row>
      </RowGroup>,
    );
    for (const [name, hint] of [
      ["Appearance", "Follow this Mac or pick a theme"],
      ["Seconds per segment", "How often segments appear"],
    ]) {
      const control = screen.getByRole(
        name === "Appearance" ? "combobox" : "spinbutton",
        { name },
      );
      const hintId = control.getAttribute("aria-describedby");
      expect(hintId).toBeTruthy();
      expect(document.getElementById(hintId!)?.textContent).toBe(hint);
    }
  });

  it("describes a button control with the row hint", () => {
    render(
      <RowGroup>
        <Row
          label="Shortcut"
          tone="danger"
          hint="Shortcut registration failed at startup: already in use"
        >
          <span className="kea-muted">Not set</span>
          <button type="button">Re-record</button>
        </Row>
      </RowGroup>,
    );
    const button = screen.getByRole("button", { name: "Re-record" });
    const hintId = button.getAttribute("aria-describedby");
    expect(hintId).toBeTruthy();
    expect(document.getElementById(hintId!)?.textContent).toBe(
      "Shortcut registration failed at startup: already in use",
    );
  });

  it("reaches a control nested in a fragment or a wrapper", () => {
    render(
      <RowGroup>
        <Row label="Updates" hint="Last checked just now">
          <>
            <button type="button">Check now</button>
          </>
          <span>
            <input aria-label="Channel" />
          </span>
        </Row>
      </RowGroup>,
    );
    for (const control of [
      screen.getByRole("button", { name: "Check now" }),
      screen.getByRole("textbox", { name: "Channel" }),
    ]) {
      const hintId = control.getAttribute("aria-describedby");
      expect(hintId).toBeTruthy();
      expect(document.getElementById(hintId!)?.textContent).toBe("Last checked just now");
    }
  });

  it("leaves a control's own aria-describedby alone", () => {
    render(
      <>
        <span id="own-hint">Own description</span>
        <RowGroup>
          <Row label="Model id" hint="Row hint">
            <input aria-label="Model id" aria-describedby="own-hint" />
          </Row>
        </RowGroup>
      </>,
    );
    expect(
      screen.getByRole("textbox", { name: "Model id" }).getAttribute("aria-describedby"),
    ).toBe("own-hint");
  });

  it("adds no description when the row has no hint", () => {
    render(
      <RowGroup>
        <Row label="Enable Dictation">
          <Toggle checked onChange={() => {}} label="Enable Dictation" />
        </Row>
      </RowGroup>,
    );
    expect(
      screen.getByRole("switch", { name: "Enable Dictation" }).hasAttribute("aria-describedby"),
    ).toBe(false);
  });
});
