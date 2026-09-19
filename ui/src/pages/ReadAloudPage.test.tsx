import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import { featureHandlers, openAiBinding } from "../test-utils/featureWorld";
import ReadAloudPage from "./ReadAloudPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const KOKORO_VOICES = [
  { sid: 0, name: "af", language: "en-US" },
  { sid: 1, name: "af_bella", language: "en-US" },
  { sid: 7, name: "bf_emma", language: "en-GB" },
];

const SYSTEM_VOICES = [
  {
    id: "com.apple.voice.compact.en-US.Samantha",
    name: "Samantha",
    language: "en-US",
    quality: "default",
  },
  {
    id: "com.apple.voice.premium.en-US.Ava",
    name: "Ava",
    language: "en-US",
    quality: "premium",
  },
];

function mockWorld(options: Parameters<typeof featureHandlers>[0] = {}) {
  onInvoke(
    featureHandlers({
      engines: { tts: ["openai-tts", "sherpa-tts", "system-tts"] },
      ...options,
      extra: {
        preview_voice: () => undefined,
        list_onnx_voices: () => [],
        list_system_voices: () => [],
        ...(options.extra ?? {}),
      },
    }),
  );
}

/** A binding straight to a local engine, which `openAiBinding` cannot express. */
const localBinding = (engine: string, model: string | null = null) => ({
  engine_id: engine,
  model,
  provider_ref: null,
});

describe("ReadAloudPage", () => {
  beforeEach(() => resetTauriMocks());

  it("titles the page with the only h1", async () => {
    mockWorld({ bindings: { "default/tts": openAiBinding("openai-tts") } });
    render(<ReadAloudPage />);

    expect(
      await screen.findByRole("heading", { level: 1, name: "Read aloud" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

  it("warns when nothing is set up", async () => {
    mockWorld({ bindings: {} });
    render(<ReadAloudPage />);

    expect(await screen.findByText(/Nothing is set up for this yet/)).toBeTruthy();
  });

  it("warns when the cloud voice has no API key", async () => {
    mockWorld({
      hasKey: false,
      bindings: { "default/tts": openAiBinding("openai-tts") },
    });
    const onNavigate = vi.fn();
    render(<ReadAloudPage onNavigate={onNavigate} />);

    expect(await screen.findByText(/OpenAI needs an API key/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Open AI Providers" }));
    expect(onNavigate).toHaveBeenCalledWith("ai-providers");
  });

  it("shows no banner once a voice resolves", async () => {
    mockWorld({ bindings: { "default/tts": openAiBinding("openai-tts") } });
    render(<ReadAloudPage />);

    expect(await screen.findByText("Using default — OpenAI")).toBeTruthy();
    expect(screen.queryByText(/Nothing is set up for this yet/)).toBeNull();
    expect(screen.queryByText(/needs an API key/)).toBeNull();
  });

  it("writes a read-aloud override from the AI card", async () => {
    mockWorld({ bindings: { "default/tts": openAiBinding("openai-tts") } });
    render(<ReadAloudPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Change…" }));
    await userEvent.click(await screen.findByRole("button", { name: /^OpenAI voices/ }));

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toEqual({
      feature: "tts",
      slot: "tts",
      engine: "openai-tts",
      model: null,
      providerRef: "openai",
    });
  });

  it("drops the override when asked to use the default again", async () => {
    mockWorld({
      bindings: {
        "default/tts": openAiBinding("openai-tts"),
        "tts/tts": { engine_id: "sherpa-tts", model: null, provider_ref: null },
      },
    });
    render(<ReadAloudPage />);

    expect(await screen.findByText(/This feature only —/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Use default again" }));

    await waitFor(() => expect(invokeCalls("delete_binding")).toHaveLength(1));
    expect(invokeCalls("delete_binding")[0]).toEqual({ feature: "tts", slot: "tts" });
  });

  /// The dropdown used to be cloud-only. A multi-speaker local bundle has to
  /// offer its speakers, by name — the settings store a name, never the
  /// integer id the bundle addresses it by.
  it("offers a local bundle's speakers by name", async () => {
    mockWorld({
      bindings: { "default/tts": localBinding("sherpa-tts", "kokoro-en-v0.19") },
      extra: {
        list_onnx_voices: () => KOKORO_VOICES,
        list_system_voices: () => [],
        preview_voice: () => undefined,
      },
    });
    render(<ReadAloudPage />);

    const select = (await screen.findByRole("combobox", { name: "Voice" })) as HTMLSelectElement;
    await waitFor(() => expect(select.options.length).toBe(KOKORO_VOICES.length + 1));
    expect(invokeCalls("list_onnx_voices")[0]).toEqual({ modelId: "kokoro-en-v0.19" });
    expect([...select.options].map((o) => o.value)).toEqual([
      "",
      "af",
      "af_bella",
      "bf_emma",
    ]);

    await userEvent.selectOptions(select, "af_bella");
    await waitFor(() => expect(invokeCalls("set_tts_settings")).toHaveLength(1));
    expect(invokeCalls("set_tts_settings")[0]?.settings).toMatchObject({
      active_voice: "af_bella",
    });
  });

  it("offers the voices macOS has installed, with their quality tier", async () => {
    mockWorld({
      bindings: { "default/tts": localBinding("system-tts") },
      extra: {
        list_system_voices: () => SYSTEM_VOICES,
        list_onnx_voices: () => [],
        preview_voice: () => undefined,
      },
    });
    render(<ReadAloudPage />);

    const select = (await screen.findByRole("combobox", { name: "Voice" })) as HTMLSelectElement;
    await waitFor(() => expect(select.options.length).toBe(SYSTEM_VOICES.length + 1));
    const labels = [...select.options].map((o) => o.textContent);
    expect(labels).toContain("Samantha (en-US)");
    // The premium tier is the reason to use the system voices at all, so it
    // is named rather than left to be discovered.
    expect(labels).toContain("Ava (en-US, premium)");
  });

  /// A single-speaker Piper voice has nothing to choose between. An empty
  /// dropdown reading "Default" is worse than no dropdown.
  it("hides the voice row for a single-speaker local model", async () => {
    mockWorld({
      bindings: {
        "default/tts": localBinding("sherpa-tts", "vits-piper-en-us-lessac-medium"),
      },
    });
    render(<ReadAloudPage />);

    expect(await screen.findByRole("slider", { name: "Speed" })).toBeTruthy();
    expect(screen.queryByRole("combobox", { name: "Voice" })).toBeNull();
  });

  it("saves the reading speed when the slider is let go", async () => {
    mockWorld({ bindings: { "default/tts": openAiBinding("openai-tts") } });
    render(<ReadAloudPage />);

    const slider = (await screen.findByRole("slider", { name: "Speed" })) as HTMLInputElement;
    expect(slider.value).toBe("1");
    fireEvent.change(slider, { target: { value: "1.5" } });
    // Dragging alone must not write: a slider that saved per pixel would fire
    // a request per frame of the drag.
    expect(invokeCalls("set_tts_settings")).toHaveLength(0);
    expect(await screen.findByText("1.50×")).toBeTruthy();

    fireEvent.pointerUp(slider);
    await waitFor(() => expect(invokeCalls("set_tts_settings")).toHaveLength(1));
    expect(invokeCalls("set_tts_settings")[0]?.settings).toMatchObject({ speed: 1.5 });
  });

  it("plays the sample through the resolved voice", async () => {
    mockWorld({ bindings: { "default/tts": openAiBinding("openai-tts") } });
    render(<ReadAloudPage />);

    await userEvent.click(await screen.findByRole("button", { name: /Play sample/ }));

    await waitFor(() => expect(invokeCalls("preview_voice")).toHaveLength(1));
    expect(invokeCalls("preview_voice")[0]).toEqual({
      engine: "openai-tts",
      model: null,
      voice: null,
      // The sample must go to the provider the binding names, or it would
      // read a different key than the real read-aloud run does.
      providerRef: "openai",
    });
  });
});
