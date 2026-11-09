import { describe, expect, it } from "vitest";
import { displayVersion } from "./version";

describe("displayVersion", () => {
  it("shows a beta in the short form", () => {
    expect(displayVersion("0.5.6-beta.1")).toBe("0.5.6b1");
    expect(displayVersion("0.5.6-beta.12")).toBe("0.5.6b12");
  });

  it("leaves a release alone", () => {
    expect(displayVersion("0.5.5")).toBe("0.5.5");
  });

  it("leaves any other pre-release alone rather than guessing", () => {
    expect(displayVersion("0.5.6-rc.1")).toBe("0.5.6-rc.1");
  });
});
