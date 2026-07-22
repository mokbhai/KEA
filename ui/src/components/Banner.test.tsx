import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import Banner from "./Banner";

describe("Banner", () => {
  it("renders warn variant as status with message and action", () => {
    render(
      <Banner variant="warn" action={<button>View</button>}>
        Almost ready — model downloading
      </Banner>,
    );
    const banner = screen.getByRole("status");
    expect(banner.className).toContain("kea-banner--warn");
    expect(screen.getByText("Almost ready — model downloading")).toBeTruthy();
    expect(screen.getByRole("button", { name: "View" })).toBeTruthy();
  });

  it("renders error variant as alert", () => {
    render(<Banner variant="error">Provider unreachable</Banner>);
    expect(screen.getByRole("alert").className).toContain("kea-banner--error");
  });

  it("renders ok variant as status", () => {
    render(<Banner variant="ok">You're ready</Banner>);
    expect(screen.getByRole("status").className).toContain("kea-banner--ok");
  });
});
