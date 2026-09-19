import { describe, expect, it } from "vitest";
import { statusClass } from "./HistoryPanel";

describe("statusClass", () => {
  it("colours a finished run green and a failed one red", () => {
    expect(statusClass("ok")).toBe("kea-status--ok");
    expect(statusClass("success")).toBe("kea-status--ok");
    expect(statusClass("error")).toBe("kea-status--error");
    expect(statusClass("failed")).toBe("kea-status--error");
  });

  /**
   * The point of the Cancelled status. A dismissed prompt palette used to close
   * its ledger row as an error, which put a red row in History for a user
   * pressing Escape. Red has to keep meaning "something went wrong".
   */
  it("does not colour a cancelled run as a failure", () => {
    expect(statusClass("cancelled")).toBe("kea-status--muted");
    expect(statusClass("cancelled")).not.toBe("kea-status--error");
  });

  it("falls back to muted for a status this build does not know", () => {
    expect(statusClass("started")).toBe("kea-status--muted");
    expect(statusClass("something-new")).toBe("kea-status--muted");
  });
});
