import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import NotionSettings from "./NotionSettings";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);

const unset = { has_token: false, parent_page: "", parent_page_error: null };

const world = (status: Record<string, unknown> = unset) =>
  onInvoke({
    get_notion_status: () => status,
    set_notion_token: () => undefined,
    clear_notion_token: () => undefined,
    set_setting: () => undefined,
  });

describe("NotionSettings", () => {
  beforeEach(() => resetTauriMocks());

  /// The step that happens outside this app is the one that has to be written
  /// down: nothing in KEA can tell the user their page is unshared until
  /// Notion answers 404, which reads as a broken export.
  it("spells out that the page must be shared with the integration", async () => {
    world();
    render(<NotionSettings />);

    expect(await screen.findByText(/my-integrations/)).toBeTruthy();
    expect(screen.getByText(/Connections/)).toBeTruthy();
  });

  it("saves the secret and reports that it is stored", async () => {
    world();
    render(<NotionSettings />);

    const field = await screen.findByLabelText("Notion integration secret");
    await userEvent.type(field, "ntn_secret");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(invokeCalls("set_notion_token")).toEqual([{ token: "ntn_secret" }]),
    );
    // Never echoed back: the field clears and the state line is all that shows.
    expect((field as HTMLInputElement).value).toBe("");
    expect((field as HTMLInputElement).type).toBe("password");
  });

  it("offers Forget only once a token exists", async () => {
    world({ has_token: true, parent_page: "", parent_page_error: null });
    render(<NotionSettings />);

    await userEvent.click(await screen.findByRole("button", { name: "Forget" }));
    await waitFor(() => expect(invokeCalls("clear_notion_token")).toHaveLength(1));
  });

  it("saves the destination link on blur", async () => {
    world();
    render(<NotionSettings />);

    const field = await screen.findByLabelText("Notion page link");
    await userEvent.type(field, "  https://www.notion.so/Notes-0123  ");
    await userEvent.tab();

    await waitFor(() =>
      expect(invokeCalls("set_setting")).toEqual([
        {
          key: "meetings.notion.parent_page",
          value: "https://www.notion.so/Notes-0123",
        },
      ]),
    );
  });

  /// The verdict comes from the same parser the export uses, so this screen
  /// cannot approve a link the export would refuse.
  it("shows why a saved link is unusable", async () => {
    world({
      has_token: true,
      parent_page: "my notion page",
      parent_page_error: "'my notion page' does not look like a Notion page link",
    });
    render(<NotionSettings />);

    expect(
      await screen.findByText(/does not look like a Notion page link/),
    ).toBeTruthy();
  });
});
