import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CustomStyle } from "../api";
import { featureHandlers, openAiBinding } from "../test-utils/featureWorld";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import RewritePage from "./RewritePage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

/**
 * The Rewrite page against a backend that remembers: custom styles and
 * settings are stored the way the real commands store them, so a flow reads
 * back what it wrote instead of what a mock was told to say.
 */
function mockWorld(initial: { styles?: CustomStyle[]; settings?: Record<string, string> } = {}) {
  let styles = [...(initial.styles ?? [])];
  const settings: Record<string, string> = { ...(initial.settings ?? {}) };
  onInvoke(
    featureHandlers({
      engines: { llm: ["openai"] },
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
      extra: {
        get_setting: (args) => settings[(args as { key: string }).key] ?? null,
        set_setting: (args) => {
          const { key, value } = args as { key: string; value: string };
          settings[key] = value;
        },
        get_prompt_override: () => null,
        list_custom_styles: () => styles,
        upsert_custom_style: (args) => {
          const next = (args as { style: CustomStyle }).style;
          styles = [...styles.filter((s) => s.id !== next.id), next];
        },
        delete_custom_style: (args) => {
          const { id } = args as { id: string };
          styles = styles.filter((s) => s.id !== id);
          // What `StyleRepo::delete` does in the backend.
          if (settings["rewrite.active_mode"] === id) settings["rewrite.active_mode"] = "improve";
        },
        preview_rewrite: () => "Rewritten.",
      },
    }),
  );
  return { settings };
}

const picker = () => screen.findByLabelText<HTMLSelectElement>("Rewrite style");

describe("custom styles on the Rewrite page", () => {
  beforeEach(() => resetTauriMocks());
  afterEach(() => vi.restoreAllMocks());

  it("adds a style, selects it, edits it and deletes it", async () => {
    const world = mockWorld();
    render(<RewritePage />);
    const user = userEvent.setup();

    // -- add ---------------------------------------------------------------
    await user.type(await screen.findByLabelText("New style name"), "Standup");
    await user.type(
      screen.getByLabelText("New style instruction"),
      "Rewrite as three bullets",
    );
    await user.click(screen.getByRole("button", { name: "Add style" }));

    await waitFor(() => expect(invokeCalls("upsert_custom_style")).toHaveLength(1));
    const added = (invokeCalls("upsert_custom_style")[0] as { style: CustomStyle }).style;
    expect(added).toMatchObject({ name: "Standup", instruction: "Rewrite as three bullets" });
    expect(added.id).toMatch(/^style-\d+$/);
    // Listed in "Your styles" and in the Style menu.
    expect(await screen.findByRole("button", { name: "Edit Standup" })).toBeTruthy();
    expect(within(await picker()).getByRole("option", { name: "Standup" })).toBeTruthy();

    // -- select ------------------------------------------------------------
    await user.selectOptions(await picker(), "Standup");
    await waitFor(() => expect(world.settings["rewrite.active_mode"]).toBe(added.id));
    // The old second selector for a saved instruction is not written any more.
    expect(Object.keys(world.settings).join(",")).not.toMatch(/active_preset/);

    // A run now goes out under that one value.
    await user.click(screen.getByRole("button", { name: "Rewrite this" }));
    await waitFor(() => expect(invokeCalls("preview_rewrite")).toHaveLength(1));
    expect(invokeCalls("preview_rewrite")[0]).toMatchObject({
      style: added.id,
      customInstruction: null,
    });

    // -- edit --------------------------------------------------------------
    await user.click(screen.getByRole("button", { name: "Edit Standup" }));
    const instruction = screen.getByLabelText("Edited style instruction");
    await user.clear(instruction);
    await user.type(instruction, "Rewrite as two bullets");
    await user.click(screen.getByRole("button", { name: "Save style" }));

    await waitFor(() => expect(invokeCalls("upsert_custom_style")).toHaveLength(2));
    expect((invokeCalls("upsert_custom_style")[1] as { style: CustomStyle }).style).toEqual({
      id: added.id,
      name: "Standup",
      instruction: "Rewrite as two bullets",
    });
    expect(await screen.findByText("Rewrite as two bullets")).toBeTruthy();

    // -- delete (it is the active style, so it asks) -------------------------
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    await user.click(screen.getByRole("button", { name: "Delete Standup" }));

    expect(confirm).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(invokeCalls("delete_custom_style")).toHaveLength(1));
    expect(invokeCalls("delete_custom_style")[0]).toEqual({ id: added.id });
    // Gone from the list and the menu, and the menu is back on Improve.
    await waitFor(() => expect(screen.queryByRole("button", { name: "Edit Standup" })).toBeNull());
    expect(within(await picker()).queryByRole("option", { name: "Standup" })).toBeNull();
    expect((await picker()).value).toBe("improve");
    expect(world.settings["rewrite.active_mode"]).toBe("improve");
  }, 20_000);

  it("opens with the saved custom style selected", async () => {
    mockWorld({
      styles: [{ id: "style-9", name: "Pirate", instruction: "Rewrite as a pirate" }],
      settings: { "rewrite.active_mode": "style-9" },
    });
    render(<RewritePage />);

    await waitFor(async () => expect((await picker()).value).toBe("style-9"));
  });

  it("falls back to Improve when the saved style no longer exists", async () => {
    mockWorld({ settings: { "rewrite.active_mode": "style-gone" } });
    render(<RewritePage />);

    expect((await picker()).value).toBe("improve");
  });

  it("shows the backend's refusal when a style cannot be stored", async () => {
    mockWorld();
    onInvoke({
      upsert_custom_style: () => {
        throw "a style named 'Standup' already exists";
      },
    });
    render(<RewritePage />);
    const user = userEvent.setup();

    await user.type(await screen.findByLabelText("New style name"), "Standup");
    await user.type(screen.getByLabelText("New style instruction"), "x");
    await user.click(screen.getByRole("button", { name: "Add style" }));

    expect(await screen.findByText(/already exists/)).toBeTruthy();
    // And the form keeps what was typed.
    expect(screen.getByLabelText<HTMLInputElement>("New style name").value).toBe("Standup");
  });
});
