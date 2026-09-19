import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  emitTauriEvent,
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import { featureHandlers } from "../test-utils/featureWorld";
import DictationPage from "./DictationPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const whisperBinding = {
  engine_id: "whisper",
  model: "whisper-base",
  provider_ref: null,
};

function mockWorld(options: Parameters<typeof featureHandlers>[0] = {}) {
  onInvoke(
    featureHandlers({
      engines: { stt: ["whisper", "openai-stt"] },
      ...options,
      extra: { get_dictation_state: () => "idle", ...(options.extra ?? {}) },
    }),
  );
}

describe("DictationPage", () => {
  beforeEach(() => resetTauriMocks());

  it("titles the page with the only h1", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);

    expect(
      await screen.findByRole("heading", { level: 1, name: "Dictation" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

  it("warns when the speech model is not downloaded and links Models", async () => {
    mockWorld({
      bindings: { "default/stt": whisperBinding },
      installedWhisper: [],
    });
    const onNavigate = vi.fn();
    render(<DictationPage onNavigate={onNavigate} />);

    expect(await screen.findByText(/Whisper Base isn't downloaded yet/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Open Models" }));
    expect(onNavigate).toHaveBeenCalledWith("models");
  });

  it("warns when nothing is set up", async () => {
    mockWorld({ bindings: {} });
    render(<DictationPage />);

    expect(await screen.findByText(/Nothing is set up for this yet/)).toBeTruthy();
  });

  it("shows no banner once the model is installed", async () => {
    mockWorld({
      bindings: { "default/stt": whisperBinding },
      installedWhisper: ["whisper-base"],
    });
    render(<DictationPage />);

    expect(
      await screen.findByText("Using default — Whisper Base — on this Mac"),
    ).toBeTruthy();
    expect(screen.queryByText(/isn't downloaded yet/)).toBeNull();
    expect(screen.queryByText(/Nothing is set up for this yet/)).toBeNull();
  });

  it("writes a dictation-scoped override from the AI card", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Change…" }));
    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toEqual({
      feature: "dictation",
      slot: "stt",
      engine: "whisper",
      model: "whisper-base",
      providerRef: null,
    });
  });

  it("drops the override when asked to use the default again", async () => {
    mockWorld({
      bindings: {
        "default/stt": whisperBinding,
        "dictation/stt": { engine_id: "openai-stt", model: "whisper-1", provider_ref: "openai" },
      },
    });
    render(<DictationPage />);

    expect(await screen.findByText("This feature only — OpenAI · whisper-1")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Use default again" }));

    await waitFor(() => expect(invokeCalls("delete_binding")).toHaveLength(1));
    expect(invokeCalls("delete_binding")[0]).toEqual({
      feature: "dictation",
      slot: "stt",
    });
  });

  it("checks the saved fallback model when the binding carries none", async () => {
    // dictation.rs uses settings.active_model when the binding has no model,
    // so an undownloaded fallback blocks the feature just the same.
    mockWorld({
      bindings: { "default/stt": { engine_id: "whisper", model: null, provider_ref: null } },
      installedWhisper: [],
      extra: {
        get_dictation_state: () => "idle",
        get_dictation_settings: () => ({
          post_process: false,
          active_model: "whisper-base",
          hold_to_talk: false,
          input_device: null,
          preroll: true,
        }),
      },
    });
    render(<DictationPage />);

    expect(await screen.findByText(/Whisper Base isn't downloaded yet/)).toBeTruthy();
  });

  it("warns about the clean-up AI only while clean-up is on", async () => {
    // Clean-up on, speech to text fine, but no LLM default and two candidates:
    // dictation.rs would fail the whole run and discard the transcript.
    mockWorld({
      bindings: { "default/stt": whisperBinding },
      engines: { stt: ["whisper"], llm: ["openai", "openai-compatible"] },
      extra: {
        get_dictation_state: () => "idle",
        get_dictation_settings: () => ({
          post_process: true,
          active_model: null,
          hold_to_talk: false,
          input_device: null,
          preroll: true,
        }),
      },
    });
    render(<DictationPage />);

    expect(
      await screen.findByText(/Clean-up \(uses the Rewrite AI\) —/),
    ).toBeTruthy();
    expect(screen.getByText(/Nothing is set up for this yet/)).toBeTruthy();
  });

  it("does not mention the clean-up AI while clean-up is off", async () => {
    mockWorld({
      bindings: { "default/stt": whisperBinding },
      engines: { stt: ["whisper"], llm: ["openai", "openai-compatible"] },
    });
    render(<DictationPage />);

    expect(
      await screen.findByText("Using default — Whisper Base — on this Mac"),
    ).toBeTruthy();
    expect(screen.queryByText(/Clean-up \(uses the Rewrite AI\)/)).toBeNull();
    expect(screen.queryByText(/Nothing is set up for this yet/)).toBeNull();
  });

  it("picks up the clean-up dependency when the toggle is switched on", async () => {
    mockWorld({
      bindings: { "default/stt": whisperBinding },
      engines: { stt: ["whisper"], llm: ["openai", "openai-compatible"] },
    });
    render(<DictationPage />);

    await userEvent.click(
      await screen.findByRole("switch", { name: "Clean up text with AI" }),
    );

    expect(
      await screen.findByText(/Clean-up \(uses the Rewrite AI\) —/),
    ).toBeTruthy();
  });

  it("reports a lookup failure instead of inventing a cause", async () => {
    mockWorld({
      bindings: { "default/stt": whisperBinding },
      extra: {
        get_dictation_state: () => "idle",
        list_stt_engines: () => {
          throw new Error("ipc channel closed");
        },
      },
    });
    render(<DictationPage />);

    expect(
      await screen.findByText(/Couldn't check the AI setup for this feature/),
    ).toBeTruthy();
    // Without the engine list we cannot know the choice is unavailable.
    expect(screen.queryByText(/isn't available in this version/)).toBeNull();
    expect(screen.queryByText(/Nothing is set up for this yet/)).toBeNull();
  });

  it("saves the AI clean-up toggle", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);

    await userEvent.click(
      await screen.findByRole("switch", { name: "Clean up text with AI" }),
    );

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_dictation_settings")[0]).toEqual({
      settings: {
        post_process: true,
        active_model: null,
        hold_to_talk: false,
        input_device: null,
        preroll: true,
      },
    });
  });

  it("offers hold-to-talk beside the shortcut and saves it", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);

    const toggle = await screen.findByRole("switch", { name: "Hold to talk" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    await userEvent.click(toggle);

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_dictation_settings")[0]).toEqual({
      settings: {
        post_process: false,
        active_model: null,
        hold_to_talk: true,
        input_device: null,
        preroll: true,
      },
    });
  });

  it("lists the input devices and saves the one picked", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);

    const picker = (await screen.findByRole("combobox", {
      name: "Input device",
    })) as HTMLSelectElement;
    await waitFor(() =>
      expect(screen.getByRole("option", { name: "Yeti" })).toBeTruthy(),
    );
    // Empty is the "System default" option: nothing is saved yet.
    expect(picker.value).toBe("");

    await userEvent.selectOptions(picker, "Yeti");

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_dictation_settings")[0]).toEqual({
      settings: {
        post_process: false,
        active_model: null,
        hold_to_talk: false,
        input_device: "Yeti",
        preroll: true,
      },
    });
  });

  it("keeps showing a saved device that is not plugged in", async () => {
    // Otherwise the dropdown would fall back to "System default" and look
    // like the user had chosen it.
    mockWorld({
      extra: {
        get_dictation_state: () => "idle",
        get_dictation_settings: () => ({
          post_process: false,
          active_model: null,
          hold_to_talk: false,
          input_device: "Road Caster",
          preroll: true,
        }),
      },
    });
    render(<DictationPage />);

    const picker = (await screen.findByRole("combobox", {
      name: "Input device",
    })) as HTMLSelectElement;
    await waitFor(() => expect(picker.value).toBe("Road Caster"));
    expect(screen.getByRole("option", { name: /Road Caster \(not connected\)/ })).toBeTruthy();
  });

  it("drives the microphone test and follows the backend when it stops itself", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);

    const toggle = await screen.findByRole("switch", { name: "Test microphone" });
    await userEvent.click(toggle);
    await waitFor(() => expect(invokeCalls("start_input_preview")).toHaveLength(1));
    expect(toggle.getAttribute("aria-checked")).toBe("true");

    // The preview times out, or a hotkey takes the microphone: the toggle has
    // to follow, because nobody clicked it off.
    await act(async () => {
      emitTauriEvent("dictation:preview", { active: false });
    });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
  });

  it("says which microphone it fell back to", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);
    await screen.findByRole("combobox", { name: "Input device" });

    await act(async () => {
      emitTauriEvent("dictation:device_fallback", {
        requested: "Yeti",
        using: "MacBook Air Microphone",
      });
    });

    expect(
      screen.getByText(/Yeti isn't connected — recording from MacBook Air Microphone/),
    ).toBeTruthy();
  });

  it("saves the first-word preroll toggle", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);

    const toggle = await screen.findByRole("switch", { name: "Catch the first word" });
    // The preroll is the one dictation setting that starts on.
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    await userEvent.click(toggle);

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_dictation_settings")[0]).toEqual({
      settings: {
        post_process: false,
        active_model: null,
        hold_to_talk: false,
        input_device: null,
        preroll: false,
      },
    });
  });

  it("names the locked state rather than calling it processing", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);
    await screen.findByRole("heading", { level: 1, name: "Dictation" });

    await act(async () => {
      emitTauriEvent("dictation:state", { state: "locked" });
    });

    expect(screen.getByText("Locked")).toBeTruthy();
  });

  it("shows hold-to-talk already on when it is saved", async () => {
    mockWorld({
      bindings: { "default/stt": whisperBinding },
      extra: {
        get_dictation_state: () => "idle",
        get_dictation_settings: () => ({
          post_process: false,
          active_model: null,
          hold_to_talk: true,
          input_device: null,
          preroll: true,
        }),
      },
    });
    render(<DictationPage />);

    await waitFor(() =>
      expect(
        screen.getByRole("switch", { name: "Hold to talk" }).getAttribute("aria-checked"),
      ).toBe("true"),
    );
  });
});
