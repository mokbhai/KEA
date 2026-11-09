import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CustomStyle } from "../api";
import type { RewriteSettingsController } from "../hooks/useRewriteSettings";
import SettingsForm from "./SettingsForm";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

/**
 * The form is presentational, so it is tested against a hand-built controller
 * rather than the hook — what matters here is which rows appear for which
 * style and which handler a change reaches.
 */
function controller(
  overrides: Partial<RewriteSettingsController> = {},
): RewriteSettingsController {
  return {
    settings: {
      style: "improve",
      custom_instruction: "",
      translate_target: "fr",
    },
    parameter: null,
    styles: [],
    translateTargets: [],
    promptOverride: "",
    loading: false,
    busy: false,
    status: null,
    chooseStyle: vi.fn(),
    chooseTranslateTarget: vi.fn(),
    addTranslateTarget: vi.fn(async () => {}),
    removeTranslateTarget: vi.fn(async () => {}),
    editCustomInstruction: vi.fn(),
    commitCustomInstruction: vi.fn(),
    editPromptOverride: vi.fn(),
    savePromptOverride: vi.fn(async () => {}),
    addStyle: vi.fn(async () => true),
    saveStyle: vi.fn(async () => true),
    removeStyle: vi.fn(async () => {}),
    ...overrides,
  };
}

const translating = (target = "fr") =>
  controller({
    settings: {
      style: "translate",
      custom_instruction: "",
      translate_target: target,
    },
  });

const style = (over: Partial<CustomStyle> = {}): CustomStyle => ({
  id: "style-1",
  name: "Standup",
  instruction: "Rewrite as three bullets",
  ...over,
});

const optionLabels = (select: HTMLElement) =>
  within(select)
    .getAllByRole("option")
    .map((o) => o.textContent);

afterEach(() => vi.restoreAllMocks());

describe("SettingsForm", () => {
  it("offers the target language only while translating", () => {
    const { rerender } = render(<SettingsForm rewrite={controller()} />);
    expect(screen.queryByLabelText("Target language")).toBeNull();

    rerender(<SettingsForm rewrite={translating()} />);
    expect(screen.getByLabelText<HTMLSelectElement>("Target language").value).toBe("fr");
  });

  it("saves the language that was picked", async () => {
    const rewrite = translating();
    render(<SettingsForm rewrite={rewrite} />);

    await userEvent.selectOptions(screen.getByLabelText("Target language"), "de");

    expect(rewrite.chooseTranslateTarget).toHaveBeenCalledWith("de");
  });

  it("shows a tag it does not list rather than re-targeting silently", () => {
    // A target inherited from the system can be any well-formed tag; dropping
    // it from the picker would make the select fall back to its first option
    // and quietly translate into the wrong language.
    render(<SettingsForm rewrite={translating("pt-PT")} />);

    expect(screen.getByLabelText<HTMLSelectElement>("Target language").value).toBe("pt-PT");
  });

  it("says which placeholder a translate prompt override must keep", () => {
    render(<SettingsForm rewrite={translating()} />);

    expect(screen.getByText("{{target_language}}")).toBeTruthy();
  });

  it("keeps the Ask KEA instruction out of the other modes", () => {
    render(<SettingsForm rewrite={translating()} />);

    expect(screen.queryByLabelText("Custom instruction")).toBeNull();
  });

  describe("the Style menu", () => {
    it("lists the built-ins, then the user's styles, then the add entry", () => {
      render(
        <SettingsForm
          rewrite={controller({ styles: [style(), style({ id: "style-2", name: "Pirate" })] })}
        />,
      );

      expect(optionLabels(screen.getByLabelText("Rewrite style"))).toEqual([
        "Improve",
        "Fix grammar",
        "Professional",
        "Concise",
        "Friendly",
        "Ask KEA",
        "Translate",
        "Standup",
        "Pirate",
        "+ New style…",
      ]);
    });

    it("never offers dictation's internal audio refinement", () => {
      render(<SettingsForm rewrite={controller()} />);

      const labels = optionLabels(screen.getByLabelText("Rewrite style"));
      expect(labels.join("|")).not.toMatch(/audio/i);
    });

    it("has no second selector for a saved instruction", () => {
      render(<SettingsForm rewrite={controller({ styles: [style()] })} />);

      expect(screen.queryByLabelText(/preset/i)).toBeNull();
    });

    it("selects a custom style through the same menu and the same handler", async () => {
      const rewrite = controller({ styles: [style()] });
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.selectOptions(screen.getByLabelText("Rewrite style"), "style-1");

      expect(rewrite.chooseStyle).toHaveBeenCalledWith("style-1");
    });

    it("shows the selected custom style as the menu's value", () => {
      render(
        <SettingsForm
          rewrite={controller({
            styles: [style()],
            settings: { style: "style-1", custom_instruction: "", translate_target: "fr" },
          })}
        />,
      );

      expect(screen.getByLabelText<HTMLSelectElement>("Rewrite style").value).toBe("style-1");
      // A custom style carries its whole prompt: no mode parameter rows, and
      // no built-in template to override.
      expect(screen.queryByLabelText("Target language")).toBeNull();
      expect(screen.queryByLabelText("Custom instruction")).toBeNull();
      expect(screen.queryByText("Save prompt override")).toBeNull();
    });

    it("'+ New style…' opens the add form instead of changing the style", async () => {
      const rewrite = controller();
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.selectOptions(screen.getByLabelText("Rewrite style"), "+ New style…");

      expect(rewrite.chooseStyle).not.toHaveBeenCalled();
      expect(document.activeElement).toBe(screen.getByLabelText("New style name"));
    });
  });

  describe("Your styles", () => {
    it("is a section of its own, not tucked under Advanced", () => {
      render(<SettingsForm rewrite={controller({ styles: [style()] })} />);

      const heading = screen.getByRole("heading", { name: "Your styles" });
      expect(heading.closest("details")).toBeNull();
      expect(screen.getByRole("button", { name: "Edit Standup" })).toBeTruthy();
      expect(screen.getByRole("button", { name: "Delete Standup" })).toBeTruthy();
    });

    it("says so when there are none", () => {
      render(<SettingsForm rewrite={controller()} />);

      expect(screen.getByText(/not added a style yet/)).toBeTruthy();
    });

    it("adds a style from a name and a multi-line instruction, then clears the form", async () => {
      const rewrite = controller();
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.type(screen.getByLabelText("New style name"), "  Standup ");
      await userEvent.type(
        screen.getByLabelText("New style instruction"),
        "Three bullets{Enter}Past tense",
      );
      await userEvent.click(screen.getByRole("button", { name: "Add style" }));

      expect(rewrite.addStyle).toHaveBeenCalledWith(
        "  Standup ",
        "Three bullets\nPast tense",
      );
      // Cleared only because the write succeeded.
      expect(screen.getByLabelText<HTMLInputElement>("New style name").value).toBe("");
      expect(screen.getByLabelText<HTMLTextAreaElement>("New style instruction").value).toBe("");
    });

    it("refuses an empty name or an empty instruction", async () => {
      const rewrite = controller();
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.click(screen.getByRole("button", { name: "Add style" }));
      expect(rewrite.addStyle).not.toHaveBeenCalled();

      await userEvent.type(screen.getByLabelText("New style name"), "Standup");
      await userEvent.click(screen.getByRole("button", { name: "Add style" }));
      expect(rewrite.addStyle).not.toHaveBeenCalled();
      expect((await screen.findByRole("alert")).textContent).toMatch(/instruction/i);

      // Whitespace is not content.
      await userEvent.type(screen.getByLabelText("New style instruction"), "   ");
      await userEvent.click(screen.getByRole("button", { name: "Add style" }));
      expect(rewrite.addStyle).not.toHaveBeenCalled();
    });

    it("refuses a name that is already taken, ignoring case and spaces", async () => {
      const rewrite = controller({ styles: [style()] });
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.type(screen.getByLabelText("New style name"), " standup ");
      await userEvent.type(screen.getByLabelText("New style instruction"), "x");
      await userEvent.click(screen.getByRole("button", { name: "Add style" }));

      expect(rewrite.addStyle).not.toHaveBeenCalled();
      expect((await screen.findByRole("alert")).textContent).toMatch(/already exists/);
    });

    it("refuses the name of a built-in style", async () => {
      const rewrite = controller();
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.type(screen.getByLabelText("New style name"), "Improve");
      await userEvent.type(screen.getByLabelText("New style instruction"), "x");
      await userEvent.click(screen.getByRole("button", { name: "Add style" }));

      expect(rewrite.addStyle).not.toHaveBeenCalled();
      expect((await screen.findByRole("alert")).textContent).toMatch(/already exists/);
    });

    it("edits a style in place and writes the whole style back under its id", async () => {
      const rewrite = controller({ styles: [style()] });
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.click(screen.getByRole("button", { name: "Edit Standup" }));
      const instruction = screen.getByLabelText<HTMLTextAreaElement>("Edited style instruction");
      expect(instruction.value).toBe("Rewrite as three bullets");
      await userEvent.clear(instruction);
      await userEvent.type(instruction, "Two bullets");
      await userEvent.click(screen.getByRole("button", { name: "Save style" }));

      expect(rewrite.saveStyle).toHaveBeenCalledWith({
        id: "style-1",
        name: "Standup",
        instruction: "Two bullets",
      });
    });

    it("lets a style keep its own name while it is edited", async () => {
      const rewrite = controller({ styles: [style()] });
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.click(screen.getByRole("button", { name: "Edit Standup" }));
      await userEvent.click(screen.getByRole("button", { name: "Save style" }));

      expect(rewrite.saveStyle).toHaveBeenCalled();
      expect(screen.queryByRole("alert")).toBeNull();
    });

    it("deletes a style that is not in use without asking", async () => {
      const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
      const rewrite = controller({ styles: [style()] });
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.click(screen.getByRole("button", { name: "Delete Standup" }));

      expect(confirm).not.toHaveBeenCalled();
      expect(rewrite.removeStyle).toHaveBeenCalledWith("style-1");
    });

    it("asks before deleting the active style, and keeps it on 'cancel'", async () => {
      const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
      const rewrite = controller({
        styles: [style()],
        settings: { style: "style-1", custom_instruction: "", translate_target: "fr" },
      });
      render(<SettingsForm rewrite={rewrite} />);

      await userEvent.click(screen.getByRole("button", { name: "Delete Standup" }));
      expect(confirm).toHaveBeenCalledTimes(1);
      expect(rewrite.removeStyle).not.toHaveBeenCalled();

      confirm.mockReturnValue(true);
      await userEvent.click(screen.getByRole("button", { name: "Delete Standup" }));
      expect(rewrite.removeStyle).toHaveBeenCalledWith("style-1");
    });
  });
});
