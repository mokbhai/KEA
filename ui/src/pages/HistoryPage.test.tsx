import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, invokeMock, onInvoke, resetTauriMocks } from "../test-utils/tauri";
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

const CONVERSATION = {
  id: 3,
  action_id: 7,
  feature_id: "rewrite",
  engine_id: "openai",
  model: "gpt-4o-mini",
  provider_ref: "openai",
  created_at: "2026-07-17T10:00:00Z",
};

function mockHistoryWorld(overrides: Record<string, () => unknown> = {}) {
  onInvoke({
    get_setting: () => null,
    set_setting: () => undefined,
    list_actions: () => [ACTION],
    list_conversations: () => [CONVERSATION],
    list_messages: () => [],
    get_action: () => ACTION_DETAIL,
    delete_conversation: () => undefined,
    ...overrides,
  });
}

describe("HistoryPage", () => {
  beforeEach(() => resetTauriMocks());
  afterEach(() => vi.restoreAllMocks());

  it("titles the page with the only h1", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

    expect(
      await screen.findByRole("heading", { level: 1, name: "History" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
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

  it("keeps the action search filter working", async () => {
    mockHistoryWorld();
    render(<HistoryPage />);

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
    render(<HistoryPage />);

    // The row is clickable, but the tab stop is what a keyboard user reaches.
    const select = await screen.findByRole("button", { name: "Show action 7" });
    select.focus();
    expect(document.activeElement).toBe(select);
    await userEvent.keyboard("{Enter}");

    expect(await screen.findByRole("heading", { level: 2, name: "Detail" })).toBeTruthy();
    expect(screen.getByText("Server unreachable")).toBeTruthy();
  });

  it("deletes a conversation from its own labelled button", async () => {
    mockHistoryWorld();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<HistoryPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Conversations" }));
    await userEvent.click(
      await screen.findByRole("button", { name: "Delete conversation 3" }),
    );

    await waitFor(() => expect(invokeCalls("delete_conversation")).toHaveLength(1));
    expect(invokeCalls("delete_conversation")[0]).toEqual({ id: 3 });
  });

  it("surfaces a load failure as an error banner", async () => {
    mockHistoryWorld({
      list_actions: () => {
        throw new Error("database is locked");
      },
    });
    render(<HistoryPage />);

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("database is locked");
  });

  it("says so when nothing has been recorded", async () => {
    mockHistoryWorld({ list_actions: () => [], list_conversations: () => [] });
    render(<HistoryPage />);

    expect(await screen.findByText(/No actions recorded yet/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Conversations" }));
    expect(screen.getByText(/No conversations stored yet/)).toBeTruthy();
    expect(invokeMock).toHaveBeenCalled();
  });
});
