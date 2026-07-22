import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Row, RowGroup } from "./SettingsRow";

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
});
