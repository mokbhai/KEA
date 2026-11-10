import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  emitTauriEvent,
  invokeCalls,
  invokeMock,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import HistoryPage from "./HistoryPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const ACTION = {
  id: 7,
  feature_id: "rewrite",
  command: "rewrite_selection",
  engine_id: "openai",
  status: "error",
};

const ACTION_DETAIL = {
  ...ACTION,
  model: "gpt-4o-mini",
  provider_ref: "openai",
  error: "Server unreachable",
  started_at: "2026-07-17T10:00:00Z",
  finished_at: "2026-07-17T10:00:01Z",
};

/** A dictation that was cleaned up: both what was said and what was pasted. */
const CLEANED = {
  id: 12,
  action_id: 40,
  feature_id: "dictation",
  engine_id: "whisper",
  model: null,
  created_at: "2026-07-17 10:05:00",
  input: "um so the meeting is at three",
  output: "The meeting is at three.",
};

/** A dictation pasted as heard. */
const PLAIN = {
  id: 11,
  action_id: 39,
  feature_id: "dictation",
  engine_id: "whisper",
  model: null,
  created_at: "2026-07-17 10:01:00",
  input: "send the invoice on friday",
  output: null,
};

const REWRITE = {
  id: 3,
  action_id: 7,
  feature_id: "rewrite",
  engine_id: "openai",
  model: "gpt-4o-mini",
  created_at: "2026-07-17 09:00:00",
  input: "pls fix asap",
  output: "Please fix this as soon as you can.",
};

const ENTRIES = [CLEANED, PLAIN, REWRITE];

function mockHistoryWorld(overrides: Record<string, (args?: any) => unknown> = {}) {
  onInvoke({
    get_setting: () => null,
    set_setting: () => undefined,
    list_history: () => ENTRIES,
    list_actions: () => [ACTION],
    get_action: () => ACTION_DETAIL,
    delete_conversation: () => undefined,
    ...overrides,
  });
}

/** The row whose words are `text`, as the list item holding both its buttons. */
async function rowFor(text: string): Promise<HTMLElement> {
  const list = await screen.findByRole("region", { name: "Past text" });
  const words = await within(list).findByText(text);
  return words.closest("li")!;
}

describe("HistoryPage", () => {
  let writeText: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    resetTauriMocks();
    writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
  });
  afterEach(() => vi.restoreAllMocks());

  it("titles the page with the only h1", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

    expect(
      await screen.findByRole("heading", { level: 1, name: "History" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

  it("shows the words of every entry in the list itself", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

    const list = await screen.findByRole("region", { name: "Past text" });
    // What was pasted, not what was heard, when the two differ.
    expect(await within(list).findByText("The meeting is at three.")).toBeTruthy();
    expect(within(list).getByText("send the invoice on friday")).toBeTruthy();
    expect(within(list).getByText("Please fix this as soon as you can.")).toBeTruthy();
    expect(within(list).queryByText("um so the meeting is at three")).toBeNull();
  });

  it("copies a row's text in one click without opening it", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

    const row = await rowFor("send the invoice on friday");
    await userEvent.click(within(row).getByRole("button", { name: /^Copy/ }));

    expect(writeText).toHaveBeenCalledWith("send the invoice on friday");
    expect(await within(row).findByRole("button", { name: /^Copy/ })).toBeTruthy();
    expect(within(row).getByText("Copied")).toBeTruthy();
    // Copying is not selecting: the newest entry stays open on the right.
    const detail = screen.getByRole("region", { name: "Selected entry" });
    expect(within(detail).getByText("The meeting is at three.")).toBeTruthy();
  });

  it("opens the newest entry beside the list, both versions copyable", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

    const detail = await screen.findByRole("region", { name: "Selected entry" });
    expect(
      await within(detail).findByRole("heading", { level: 2, name: "Dictation" }),
    ).toBeTruthy();
    expect(within(detail).getByRole("heading", { level: 3, name: "Cleaned up" })).toBeTruthy();
    expect(
      within(detail).getByRole("heading", { level: 3, name: "As you said it" }),
    ).toBeTruthy();

    await userEvent.click(within(detail).getByRole("button", { name: "Copy as you said it" }));
    expect(writeText).toHaveBeenCalledWith("um so the meeting is at three");
  });

  it("opens whichever row is clicked", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

    const row = await rowFor("Please fix this as soon as you can.");
    await userEvent.click(within(row).getByRole("button", { name: /Please fix this/ }));

    const detail = screen.getByRole("region", { name: "Selected entry" });
    expect(within(detail).getByRole("heading", { level: 2, name: "Rewrite" })).toBeTruthy();
    expect(within(detail).getByRole("heading", { level: 3, name: "Original" })).toBeTruthy();
    expect(within(detail).getByText("pls fix asap")).toBeTruthy();
  });

  it("walks the rows with the arrow keys", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

    const first = await rowFor("The meeting is at three.");
    within(first).getByRole("button", { name: /The meeting is at three/ }).focus();
    await userEvent.keyboard("{ArrowDown}");

    const detail = screen.getByRole("region", { name: "Selected entry" });
    expect(within(detail).getByText("send the invoice on friday")).toBeTruthy();
    expect(within(detail).getByRole("heading", { level: 3, name: "Transcript" })).toBeTruthy();
  });

  it("filters by feature and searches as you type", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Rewrite" }));
    await waitFor(() =>
      expect(invokeCalls("list_history").some((a) => a?.feature === "rewrite")).toBe(true),
    );

    await userEvent.type(screen.getByRole("searchbox", { name: "Search history" }), "invoice");
    await waitFor(() =>
      expect(
        invokeCalls("list_history").some(
          (a) => a?.feature === "rewrite" && a?.query === "invoice",
        ),
      ).toBe(true),
    );
  });

  it("deletes the open entry after asking", async () => {
    mockHistoryWorld();
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<HistoryPage />);

    const detail = await screen.findByRole("region", { name: "Selected entry" });
    await userEvent.click(await within(detail).findByRole("button", { name: "Delete" }));

    expect(confirm).toHaveBeenCalled();
    await waitFor(() => expect(invokeCalls("delete_conversation")).toEqual([{ id: 12 }]));
  });

  it("picks up new text when a dictation finishes", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);
    await rowFor("The meeting is at three.");
    const before = invokeCalls("list_history").length;

    emitTauriEvent("dictation:state", { state: "idle" });

    await waitFor(() => expect(invokeCalls("list_history").length).toBeGreaterThan(before));
  });

  it("reports a failed copy instead of pretending", async () => {
    mockHistoryWorld();
    writeText.mockRejectedValue(new Error("clipboard is busy"));
    render(<HistoryPage />);

    const row = await rowFor("send the invoice on friday");
    await userEvent.click(within(row).getByRole("button", { name: /^Copy/ }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("clipboard is busy");
    expect(within(row).queryByText("Copied")).toBeNull();
  });

  it("surfaces a load failure as an error banner", async () => {
    mockHistoryWorld({
      list_history: () => {
        throw new Error("database is locked");
      },
    });
    render(<HistoryPage />);

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("database is locked");
  });

  it("says so when nothing has been kept, and why", async () => {
    mockHistoryWorld({ list_history: () => [] });
    render(<HistoryPage />);

    expect(await screen.findByText(/Nothing here yet\. Dictate or rewrite/)).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Selected entry" })).toBeNull();
  });

  it("names the switch that is off when nothing is being kept", async () => {
    mockHistoryWorld({ list_history: () => [], get_setting: () => "false" });
    render(<HistoryPage />);

    expect(await screen.findByText(/Saving text is turned off/)).toBeTruthy();
  });

  it("persists the store-conversations row through the toggle", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

    const toggle = await screen.findByRole("switch", {
      name: "Store conversation content",
    });
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    // The row hint is announced with the control, not just shown next to it.
    expect(toggle.getAttribute("aria-describedby")).toBeTruthy();

    await userEvent.click(toggle);

    await waitFor(() =>
      expect(
        invokeCalls("set_setting").some(
          (args) =>
            args?.key === "history.store_conversations" && args?.value === "false",
        ),
      ).toBe(true),
    );
  });

  describe("activity log", () => {
    async function openLog() {
      render(<HistoryPage />);
      await userEvent.click(await screen.findByRole("button", { name: "Activity log" }));
    }

    it("keeps the action search filter working", async () => {
      mockHistoryWorld();
      await openLog();

      await userEvent.type(
        await screen.findByRole("textbox", { name: "Search actions" }),
        "rewrite",
      );
      await userEvent.click(screen.getByRole("button", { name: "Search" }));

      await waitFor(() =>
        expect(invokeCalls("list_actions").some((args) => args?.query === "rewrite")).toBe(
          true,
        ),
      );

      await userEvent.click(screen.getByRole("button", { name: "Clear" }));
      await waitFor(() =>
        expect(
          invokeCalls("list_actions").filter((args) => args?.query === null).length,
        ).toBeGreaterThan(1),
      );
    });

    it("opens an action detail from the keyboard", async () => {
      mockHistoryWorld();
      await openLog();

      // The row is clickable, but the tab stop is what a keyboard user reaches.
      const select = await screen.findByRole("button", { name: "Show action 7" });
      select.focus();
      expect(document.activeElement).toBe(select);
      await userEvent.keyboard("{Enter}");

      expect(await screen.findByText("Server unreachable")).toBeTruthy();
      expect(screen.getByRole("heading", { level: 2, name: "Detail" })).toBeTruthy();
    });

    it("says so when nothing has been recorded", async () => {
      mockHistoryWorld({ list_actions: () => [] });
      await openLog();

      expect(await screen.findByText(/No actions recorded yet/)).toBeTruthy();
      expect(invokeMock).toHaveBeenCalled();
    });
  });
});
