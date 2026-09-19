import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { RewritePreset } from "../api";
import type { RewriteSettingsController } from "../hooks/useRewriteSettings";
import SettingsForm from "./SettingsForm";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

/**
 * The form is presentational, so it is tested against a hand-built controller
 * rather than the hook — what matters here is which rows appear for which mode
 * and which handler a change reaches.
 */
function controller(
  overrides: Partial<RewriteSettingsController> = {},
): RewriteSettingsController {
  return {
    settings: {
      mode: "improve",
      preset_id: null,
      custom_instruction: "",
      translate_target: "fr",
    },
    parameter: null,
    presets: [],
    translateTargets: [],
    promptOverride: "",
    loading: false,
    busy: false,
    status: null,
    chooseMode: vi.fn(),
    choosePreset: vi.fn(),
    chooseTranslateTarget: vi.fn(),
    addTranslateTarget: vi.fn(async () => {}),
    removeTranslateTarget: vi.fn(async () => {}),
    editCustomInstruction: vi.fn(),
    commitCustomInstruction: vi.fn(),
    editPromptOverride: vi.fn(),
    savePromptOverride: vi.fn(async () => {}),
    addPreset: vi.fn(async () => true),
    savePreset: vi.fn(async () => {}),
    removePreset: vi.fn(async () => {}),
    ...overrides,
  };
}

const translating = (target = "fr") =>
  controller({
    settings: {
      mode: "translate",
      preset_id: null,
      custom_instruction: "",
      translate_target: target,
    },
  });

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

  const preset = (over: Partial<RewritePreset> = {}): RewritePreset => ({
    id: "p1",
    name: "Grammar",
    instruction: "Fix the grammar",
    llm_engine_id: null,
    llm_model: null,
    llm_provider_ref: null,
    ...over,
  });

  it("shows a preset with no AI of its own as inheriting the Rewrite AI", async () => {
    const rewrite = controller({ presets: [preset()] });
    render(<SettingsForm rewrite={rewrite} />);

    const select = await screen.findByLabelText<HTMLSelectElement>("AI engine for Grammar");
    expect(select.value).toBe("");
    // The model and provider mean nothing without an engine, so they are not
    // offered until one is chosen.
    expect(screen.getByLabelText<HTMLInputElement>("Model for Grammar").disabled).toBe(true);
  });

  it("writes the whole preset back when its engine changes", async () => {
    const rewrite = controller({
      presets: [preset({ llm_engine_id: "openai", llm_model: "gpt-4o" })],
    });
    render(<SettingsForm rewrite={rewrite} />);

    const model = await screen.findByLabelText<HTMLInputElement>("Model for Grammar");
    expect(model.disabled).toBe(false);

    await userEvent.type(model, "-mini");

    // The whole row goes back, not just the field that changed: `upsert_preset`
    // takes the whole preset, and a partial write would drop the instruction.
    expect(rewrite.savePreset).toHaveBeenCalledWith(
      expect.objectContaining({
        id: "p1",
        name: "Grammar",
        instruction: "Fix the grammar",
        llm_engine_id: "openai",
      }),
    );
  });

  it("says which override wins when an app rule also names an AI", async () => {
    // The precedence is a decision the user cannot discover by trying it —
    // the rule only applies in one app — so the editor states it.
    render(<SettingsForm rewrite={controller({ presets: [preset()] })} />);

    expect(await screen.findByText(/app rule that names its own AI wins/i)).toBeTruthy();
  });
});
