import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import LogsPage from "./LogsPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

describe("LogsPage", () => {
  beforeEach(() => resetTauriMocks());

  it("titles the page with the only h1 and shows the tail", async () => {
    onInvoke({ tail_logs: () => "INFO kea started", open_log_folder: () => undefined });
    render(<LogsPage />);

    expect(await screen.findByRole("heading", { level: 1, name: "Logs" })).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    expect(await screen.findByText("INFO kea started")).toBeTruthy();
  });

  it("keeps refresh and open-folder working", async () => {
    onInvoke({ tail_logs: () => "", open_log_folder: () => undefined });
    render(<LogsPage />);

    await waitFor(() => expect(invokeCalls("tail_logs")).toHaveLength(1));
    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(invokeCalls("tail_logs")).toHaveLength(2));

    await userEvent.click(screen.getByRole("button", { name: "Open log folder" }));
    await waitFor(() => expect(invokeCalls("open_log_folder")).toHaveLength(1));
  });

  it("re-reads the tail on a timer while Live is on, and stops when it is off", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      onInvoke({ tail_logs: () => "INFO kea started", open_log_folder: () => undefined });
      render(<LogsPage />);
      await waitFor(() => expect(invokeCalls("tail_logs")).toHaveLength(1));

      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      await user.click(screen.getByRole("button", { name: "Live" }));

      // Polling is what makes a once-an-hour paste failure observable at all;
      // without it the user only ever sees the log after the fact.
      await vi.advanceTimersByTimeAsync(2100);
      await waitFor(() => expect(invokeCalls("tail_logs").length).toBeGreaterThan(1));

      const whileLive = invokeCalls("tail_logs").length;
      await user.click(screen.getByRole("button", { name: "Stop live" }));
      await vi.advanceTimersByTimeAsync(3000);
      expect(invokeCalls("tail_logs")).toHaveLength(whileLive);
    } finally {
      vi.useRealTimers();
    }
  });

  it("surfaces a read failure as an error banner", async () => {
    onInvoke({
      tail_logs: () => {
        throw new Error("log file missing");
      },
    });
    render(<LogsPage />);

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("log file missing");
  });
});
