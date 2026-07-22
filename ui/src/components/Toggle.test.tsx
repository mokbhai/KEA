import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import Toggle from "./Toggle";

describe("Toggle", () => {
  it("exposes switch semantics and accessible name", () => {
    render(<Toggle checked={false} onChange={() => {}} label="Enable Dictation" />);
    const sw = screen.getByRole("switch", { name: "Enable Dictation" });
    expect(sw.getAttribute("aria-checked")).toBe("false");
  });

  it("reports the inverted value on click", async () => {
    const onChange = vi.fn();
    render(<Toggle checked={true} onChange={onChange} label="Enable" />);
    await userEvent.click(screen.getByRole("switch", { name: "Enable" }));
    expect(onChange).toHaveBeenCalledWith(false);
  });

  it("is keyboard operable (Space)", async () => {
    const onChange = vi.fn();
    render(<Toggle checked={false} onChange={onChange} label="Enable" />);
    screen.getByRole("switch", { name: "Enable" }).focus();
    await userEvent.keyboard(" ");
    expect(onChange).toHaveBeenCalledWith(true);
  });

  it("does not fire when disabled", async () => {
    const onChange = vi.fn();
    render(<Toggle checked={false} onChange={onChange} label="Enable" disabled />);
    await userEvent.click(screen.getByRole("switch", { name: "Enable" }));
    expect(onChange).not.toHaveBeenCalled();
  });
});
