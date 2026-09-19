import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  emitTauriEvent,
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import { featureHandlers, WHISPER_CATALOG } from "../test-utils/featureWorld";
import DictationPage from "./DictationPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const whisperBinding = {
  engine_id: "whisper",
  model: "whisper-base",
  provider_ref: null,
};

/** The one whisper build a language can be chosen for. */
const TURBO = {
  id: "whisper-turbo",
  display_name: "Whisper Large v3 Turbo",
  language: "multilingual",
  url: "",
  size_bytes: 574_041_195,
  sha256: "",
  deprecated: false,
};

/** A world whose speech-to-text is `engine` on a catalog including TURBO. */
function multilingualWorld(engine: string, model: string | null = TURBO.id) {
  return {
    bindings: { "default/stt": { engine_id: engine, model, provider_ref: null } },
    engines: { stt: [engine] },
    installedWhisper: ["whisper-base", TURBO.id],
    installedOnnx: [TURBO.id],
    extra: {
      list_whisper_models: () => [...WHISPER_CATALOG, TURBO],
      list_onnx_models: () => [TURBO],
    },
  };
}

function mockWorld(options: Parameters<typeof featureHandlers>[0] = {}) {
  onInvoke(
    featureHandlers({
      engines: { stt: ["whisper", "openai-stt"] },
      ...options,
      extra: { get_dictation_state: () => "idle", ...(options.extra ?? {}) },
    }),
  );
}

/** Two installed live-preview models, plus one that is only in the catalog. */
const STREAM_20M = {
  id: "streaming-zipformer-en-20m",
  display_name: "Zipformer English 20M",
  language: "en-US",
  url: "",
  size_bytes: 80 * 1024 * 1024,
  sha256: "",
  deprecated: false,
};

const STREAM_80M = {
  id: "streaming-zipformer-en-80m",
  display_name: "Zipformer English 80M",
  language: "en-US",
  url: "",
  size_bytes: 320 * 1024 * 1024,
  sha256: "",
  deprecated: false,
};

/**
 * A world whose streaming catalog holds both models but has only `installed`
 * on disk, with `settings` seeded into the generic settings table. The kind is
 * honoured so the streaming catalog cannot leak into the speech-to-text one.
 */
function streamingWorld(
  installed: string[],
  settings: Record<string, string> = {},
) {
  return {
    bindings: { "default/stt": whisperBinding },
    extra: {
      get_dictation_state: () => "idle",
      list_onnx_models: (args?: Record<string, unknown>) =>
        args?.kind === "streaming" ? [STREAM_20M, STREAM_80M] : [],
      list_installed_onnx_models: (args?: Record<string, unknown>) =>
        args?.kind === "streaming" ? installed : [],
      get_setting: (args?: Record<string, unknown>) =>
        settings[args?.key as string] ?? null,
      set_setting: () => undefined,
    },
  };
}

/** The set_setting write for one key, as the backend would see it. */
const wroteSetting = (key: string) =>
  invokeCalls("set_setting").filter((c) => c?.key === key);

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
          language: null,
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
          language: null,
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
        language: null,
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
        language: null,
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
        language: null,
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
          language: null,
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
        language: null,
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
          language: null,
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
  it("offers a language for a multilingual Whisper model and saves the tag", async () => {
    mockWorld(multilingualWorld("whisper"));
    render(<DictationPage />);

    const picker = (await screen.findByRole("combobox", {
      name: "Dictation language",
    })) as HTMLSelectElement;
    // Auto-detect is the default, and it is the empty option.
    expect(picker.value).toBe("");

    await userEvent.selectOptions(picker, "de");

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_dictation_settings")[0]).toEqual({
      settings: {
        post_process: false,
        active_model: null,
        hold_to_talk: false,
        input_device: null,
        preroll: true,
        language: "de",
      },
    });
  });

  it("goes back to auto-detect", async () => {
    mockWorld({
      ...multilingualWorld("whisper"),
      extra: {
        ...multilingualWorld("whisper").extra,
        get_dictation_settings: () => ({
          post_process: false,
          active_model: null,
          hold_to_talk: false,
          input_device: null,
          preroll: true,
          language: "de",
        }),
      },
    });
    render(<DictationPage />);

    const picker = (await screen.findByRole("combobox", {
      name: "Dictation language",
    })) as HTMLSelectElement;
    await waitFor(() => expect(picker.value).toBe("de"));

    await userEvent.selectOptions(picker, "");

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(
      (invokeCalls("set_dictation_settings")[0] as { settings: { language: unknown } })
        .settings.language,
    ).toBeNull();
  });

  it("hides the language row for an English-only Whisper model", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);
    await screen.findByRole("switch", { name: "Hold to talk" });

    expect(screen.queryByRole("combobox", { name: "Dictation language" })).toBeNull();
    // Nothing is said about it either: an explanation for a control that is
    // not on screen is noise.
    expect(screen.queryByText(/language/i)).toBeNull();
  });

  it("hides the language row for an engine that would drop it", async () => {
    // Parakeet's transducer has no language setting: offering one would
    // re-open the defect the design review closed.
    mockWorld(multilingualWorld("parakeet"));
    render(<DictationPage />);
    await screen.findByRole("switch", { name: "Hold to talk" });

    expect(screen.queryByRole("combobox", { name: "Dictation language" })).toBeNull();
  });

  it("offers a language for the saved fallback model when the binding names none", async () => {
    // dictation.rs falls back to settings.active_model, so that is the model
    // the gate has to be read against.
    const world = multilingualWorld("whisper", null);
    mockWorld({
      ...world,
      extra: {
        ...world.extra,
        get_dictation_settings: () => ({
          post_process: false,
          active_model: TURBO.id,
          hold_to_talk: false,
          input_device: null,
          preroll: true,
          language: null,
        }),
      },
    });
    render(<DictationPage />);

    expect(
      await screen.findByRole("combobox", { name: "Dictation language" }),
    ).toBeTruthy();
  });

  it("keeps showing a saved tag this build does not list", async () => {
    const world = multilingualWorld("whisper");
    mockWorld({
      ...world,
      extra: {
        ...world.extra,
        get_dictation_settings: () => ({
          post_process: false,
          active_model: null,
          hold_to_talk: false,
          input_device: null,
          preroll: true,
          language: "cy",
        }),
      },
    });
    render(<DictationPage />);

    const picker = (await screen.findByRole("combobox", {
      name: "Dictation language",
    })) as HTMLSelectElement;
    await waitFor(() => expect(picker.value).toBe("cy"));
  });
  it("offers only the installed live-preview models, plus a real Off", async () => {
    mockWorld(streamingWorld([STREAM_20M.id]));
    render(<DictationPage />);

    const picker = (await screen.findByRole("combobox", {
      name: "Live preview model",
    })) as HTMLSelectElement;
    // Nothing saved yet, and Off is the empty option.
    await waitFor(() => expect(picker.value).toBe(""));
    expect(screen.getByRole("option", { name: "Off" })).toBeTruthy();
    expect(screen.getByRole("option", { name: "Zipformer English 20M" })).toBeTruthy();
    // In the catalog but not on disk: choosing it could only do nothing.
    expect(screen.queryByRole("option", { name: "Zipformer English 80M" })).toBeNull();
  });

  it("says the preview is a preview, not what gets typed", async () => {
    mockWorld(streamingWorld([STREAM_20M.id]));
    render(<DictationPage />);

    await screen.findByRole("combobox", { name: "Live preview model" });
    expect(
      screen.getByText(/preview only.*transcribes the whole recording again/s),
    ).toBeTruthy();
  });

  it("saves the picked live-preview model", async () => {
    mockWorld(streamingWorld([STREAM_20M.id]));
    render(<DictationPage />);

    const picker = await screen.findByRole("combobox", { name: "Live preview model" });
    await userEvent.selectOptions(picker, STREAM_20M.id);

    await waitFor(() =>
      expect(wroteSetting("dictation.streaming_model")).toHaveLength(1),
    );
    expect(wroteSetting("dictation.streaming_model")[0]?.value).toBe(STREAM_20M.id);
  });

  it("clears the model when Off is chosen", async () => {
    mockWorld(
      streamingWorld([STREAM_20M.id], { "dictation.streaming_model": STREAM_20M.id }),
    );
    render(<DictationPage />);

    const picker = (await screen.findByRole("combobox", {
      name: "Live preview model",
    })) as HTMLSelectElement;
    await waitFor(() => expect(picker.value).toBe(STREAM_20M.id));

    await userEvent.selectOptions(picker, "");

    await waitFor(() =>
      expect(wroteSetting("dictation.streaming_model")).toHaveLength(1),
    );
    // The empty string is what the backend reads as off — see
    // `streaming_model_setting` in src-tauri/src/commands.rs.
    expect(wroteSetting("dictation.streaming_model")[0]?.value).toBe("");
  });

  it("hides the dependent switches until a model is chosen", async () => {
    mockWorld(streamingWorld([STREAM_20M.id]));
    render(<DictationPage />);

    await screen.findByRole("combobox", { name: "Live preview model" });
    expect(screen.queryByRole("switch", { name: "Show the preview" })).toBeNull();
    expect(
      screen.queryByRole("switch", { name: "Type the preview if the second pass fails" }),
    ).toBeNull();
  });

  it("shows the dependent switches at their backend defaults once a model is on", async () => {
    mockWorld(
      streamingWorld([STREAM_20M.id], { "dictation.streaming_model": STREAM_20M.id }),
    );
    render(<DictationPage />);

    const show = await screen.findByRole("switch", { name: "Show the preview" });
    // Partials default on, inserting the draft after a failure defaults off.
    expect(show.getAttribute("aria-checked")).toBe("true");
    expect(
      screen
        .getByRole("switch", { name: "Type the preview if the second pass fails" })
        .getAttribute("aria-checked"),
    ).toBe("false");
  });

  it("writes the dependent switches as the strings the backend reads", async () => {
    mockWorld(
      streamingWorld([STREAM_20M.id], { "dictation.streaming_model": STREAM_20M.id }),
    );
    render(<DictationPage />);

    await userEvent.click(
      await screen.findByRole("switch", {
        name: "Type the preview if the second pass fails",
      }),
    );

    await waitFor(() =>
      expect(wroteSetting("dictation.streaming_fallback")).toHaveLength(1),
    );
    // A JSON-encoded *string*, not a bool: set_setting takes a String, and
    // that is the shape the Rust side reads back.
    expect(wroteSetting("dictation.streaming_fallback")[0]?.value).toBe("true");
    expect(wroteSetting("dictation.show_partials")[0]?.value).toBe("true");
    expect(wroteSetting("dictation.streaming_model")[0]?.value).toBe(STREAM_20M.id);
  });

  it("turns the preview display off as the string false", async () => {
    mockWorld(
      streamingWorld([STREAM_20M.id], { "dictation.streaming_model": STREAM_20M.id }),
    );
    render(<DictationPage />);

    await userEvent.click(await screen.findByRole("switch", { name: "Show the preview" }));

    await waitFor(() => expect(wroteSetting("dictation.show_partials")).toHaveLength(1));
    expect(wroteSetting("dictation.show_partials")[0]?.value).toBe("false");
  });

  it("says why the fallback is off by default", async () => {
    mockWorld(
      streamingWorld([STREAM_20M.id], { "dictation.streaming_model": STREAM_20M.id }),
    );
    render(<DictationPage />);

    await screen.findByRole("switch", { name: "Show the preview" });
    expect(screen.getByText(/which is why it starts off/)).toBeTruthy();
  });

  it("sends the user to Models when no preview model is downloaded", async () => {
    mockWorld(streamingWorld([]));
    const onNavigate = vi.fn();
    render(<DictationPage onNavigate={onNavigate} />);

    expect(await screen.findByText(/No preview model is downloaded yet/)).toBeTruthy();
    // Nothing to choose from, so no picker offering a model the user lacks.
    expect(screen.queryByRole("combobox", { name: "Live preview model" })).toBeNull();

    await userEvent.click(
      screen.getByRole("button", { name: "Open Models to download a live preview" }),
    );
    expect(onNavigate).toHaveBeenCalledWith("models");
  });

  it("keeps a saved model selectable after its files are removed", async () => {
    mockWorld(streamingWorld([], { "dictation.streaming_model": STREAM_20M.id }));
    render(<DictationPage />);

    const picker = (await screen.findByRole("combobox", {
      name: "Live preview model",
    })) as HTMLSelectElement;
    await waitFor(() => expect(picker.value).toBe(STREAM_20M.id));
    expect(
      screen.getByRole("option", { name: /streaming-zipformer-en-20m \(not downloaded\)/ }),
    ).toBeTruthy();
  });

  it("reads a model the backend cleared to JSON null as Off", async () => {
    // `clear_streaming_model_for_deleted` stores the literal `null`, which
    // get_setting's `get::<String>` cannot deserialize — so that read rejects,
    // and off is exactly what the state means.
    mockWorld({
      ...streamingWorld([STREAM_20M.id]),
      extra: {
        ...streamingWorld([STREAM_20M.id]).extra,
        get_setting: (args?: Record<string, unknown>) => {
          if (args?.key === "dictation.streaming_model") {
            throw new Error("invalid type: null, expected a string");
          }
          return null;
        },
      },
    });
    render(<DictationPage />);

    const picker = (await screen.findByRole("combobox", {
      name: "Live preview model",
    })) as HTMLSelectElement;
    await waitFor(() => expect(picker.value).toBe(""));
    expect(screen.queryByRole("switch", { name: "Show the preview" })).toBeNull();
  });

  describe("voice commands", () => {
    /**
     * A world whose dictation language and speech model are known, with the
     * generic settings table seeded. The language matters: the backend gate
     * only runs the English command table, and the page mirrors that.
     */
    function voiceWorld(
      settings: Record<string, string> = {},
      language: string | null = "en",
      model: string | null = "whisper-base",
    ) {
      return {
        bindings: {
          "default/stt": { engine_id: "whisper", model, provider_ref: null },
        },
        extra: {
          get_dictation_state: () => "idle",
          get_dictation_settings: () => ({
            post_process: false,
            active_model: null,
            hold_to_talk: false,
            input_device: null,
            preroll: true,
            language,
          }),
          get_setting: (args?: Record<string, unknown>) =>
            settings[args?.key as string] ?? null,
          set_setting: () => undefined,
        },
      };
    }

    /** The last value written for a key, decoded from its JSON. */
    const lastWrite = (key: string) => {
      const calls = wroteSetting(key);
      return calls.length ? (calls[calls.length - 1]?.value as string) : null;
    };

    it("starts off and shows no command table", async () => {
      mockWorld(voiceWorld());
      render(<DictationPage />);

      const toggle = await screen.findByRole("switch", {
        name: "Run spoken commands",
      });
      expect(toggle.getAttribute("aria-checked")).toBe("false");
      expect(screen.queryByRole("table", { name: "Spoken commands" })).toBeNull();
    });

    it("switches on with punctuation and retraction, and structure off", async () => {
      mockWorld(voiceWorld());
      render(<DictationPage />);

      await userEvent.click(
        await screen.findByRole("switch", { name: "Run spoken commands" }),
      );

      await waitFor(() =>
        expect(lastWrite("dictation.voice_commands_enabled")).toBe("true"),
      );
      // The shipping defaults, and the reason for them: "new paragraph" and
      // "new line" are things people say literally.
      const ids = JSON.parse(lastWrite("dictation.voice_commands") ?? "[]");
      expect(ids).toContain("period");
      expect(ids).toContain("scratch_that");
      expect(ids).not.toContain("new_paragraph");
      expect(ids).not.toContain("new_line");
    });

    it("reads the stored command set back, including an empty one", async () => {
      // Empty is a decision — everything off — not "never configured".
      mockWorld(
        voiceWorld({
          "dictation.voice_commands_enabled": "true",
          "dictation.voice_commands": "[]",
        }),
      );
      render(<DictationPage />);

      await screen.findByRole("table", { name: "Spoken commands" });
      for (const name of ["period", "scratch that", "new paragraph"]) {
        expect(
          screen.getByRole("switch", { name }).getAttribute("aria-checked"),
        ).toBe("false");
      }
    });

    it("turns one command on without disturbing the others", async () => {
      mockWorld(
        voiceWorld({
          "dictation.voice_commands_enabled": "true",
          "dictation.voice_commands": JSON.stringify(["period"]),
        }),
      );
      render(<DictationPage />);

      await userEvent.click(
        await screen.findByRole("switch", { name: "new paragraph" }),
      );

      await waitFor(() =>
        expect(
          JSON.parse(lastWrite("dictation.voice_commands") ?? "[]"),
        ).toEqual(["period", "new_paragraph"]),
      );
    });

    it("names the escape phrase and the words that force a literal", async () => {
      mockWorld(
        voiceWorld({ "dictation.voice_commands_enabled": "true" }),
      );
      render(<DictationPage />);

      // The escape is the user's only recourse when the rules get it wrong,
      // so it has to be on the page and not in the docs.
      expect(await screen.findByText(/literally period/)).toBeTruthy();
      expect(screen.getByText(/is always typed, not run/)).toBeTruthy();
    });

    it("says so when the language gate will stop every command", async () => {
      mockWorld(
        voiceWorld({ "dictation.voice_commands_enabled": "true" }, "de"),
      );
      render(<DictationPage />);

      expect(
        await screen.findByText(/command list is English only/),
      ).toBeTruthy();
    });

    it("stays quiet when the language is English", async () => {
      mockWorld(
        voiceWorld({ "dictation.voice_commands_enabled": "true" }, "en-US"),
      );
      render(<DictationPage />);

      await screen.findByRole("table", { name: "Spoken commands" });
      expect(screen.queryByText(/command list is English only/)).toBeNull();
    });

    it("accepts an English-only model when the language is auto-detect", async () => {
      // Mirrors model_is_english_only: whisper spells those builds ".en".
      mockWorld(
        voiceWorld(
          { "dictation.voice_commands_enabled": "true" },
          null,
          "ggml-base.en",
        ),
      );
      render(<DictationPage />);

      await screen.findByRole("table", { name: "Spoken commands" });
      expect(screen.queryByText(/command list is English only/)).toBeNull();
    });

    it("warns on auto-detect with a multilingual model", async () => {
      mockWorld(
        voiceWorld(
          { "dictation.voice_commands_enabled": "true" },
          null,
          "whisper-turbo",
        ),
      );
      render(<DictationPage />);

      expect(
        await screen.findByText(/command list is English only/),
      ).toBeTruthy();
    });
  });
});
