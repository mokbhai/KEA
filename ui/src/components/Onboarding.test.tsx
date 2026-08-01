import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  emitTauriEvent,
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import Onboarding from "./Onboarding";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const MB = 1024 * 1024;

const WHISPER_CATALOG = [
  {
    id: "whisper-base",
    display_name: "Whisper Base",
    language: "Multilingual",
    url: "",
    size_bytes: 148 * MB,
    sha256: "",
  },
];

const PARAKEET_CATALOG = [
  {
    id: "parakeet-v3",
    display_name: "Parakeet v3",
    language: "English",
    url: "",
    size_bytes: 487 * MB,
    sha256: "",
    kind: "Parakeet",
  },
];

const TTS_CATALOG = [
  {
    id: "vits-piper-amy",
    display_name: "Amy",
    language: "en_US",
    url: "",
    size_bytes: 63 * MB,
    sha256: "",
    kind: "TtsVits",
  },
];

type WorldOptions = {
  hasKey?: boolean;
  existingBindings?: Record<string, unknown>;
};

function mockWizardWorld({ hasKey = true, existingBindings = {} }: WorldOptions = {}) {
  const bindings = new Map(Object.entries(existingBindings));
  onInvoke({
    get_all_permission_statuses: () => [
      { kind: "microphone", status: "Granted" },
      { kind: "screen_recording", status: "Denied" },
      { kind: "accessibility", status: "Unknown" },
    ],
    request_permission: () => "Granted",
    open_accessibility_settings: () => undefined,
    has_credential: () => hasKey,
    set_credential: () => undefined,
    test_provider: () => ({ ok: true, message: "Connected" }),
    list_providers: () => [
      { provider_ref: "openai", name: "OpenAI", built_in: true },
      { provider_ref: "local-llm", name: "Local server", built_in: true },
    ],
    get_provider_config: () => null,
    get_binding: (args) => bindings.get(`${args?.feature}/${args?.slot}`) ?? null,
    set_binding: (args) => {
      bindings.set(`${args?.feature}/${args?.slot}`, {
        engine_id: args?.engine,
        model: args?.model ?? null,
        provider_ref: args?.providerRef ?? null,
      });
    },
    list_stt_engines: () => [{ id: "whisper", models: [] }],
    list_tts_engines: () => [{ id: "sherpa-tts", models: [] }],
    list_whisper_models: () => WHISPER_CATALOG,
    list_installed_whisper_models: () => [],
    list_onnx_models: (args) => (args?.kind === "tts" ? TTS_CATALOG : PARAKEET_CATALOG),
    list_installed_onnx_models: () => [],
    download_whisper_model: () => undefined,
    download_onnx_model: () => undefined,
    get_dictation_settings: () => ({ post_process: false, active_model: null }),
    set_dictation_settings: () => undefined,
    get_tts_settings: () => ({ active_voice: null, active_model: null }),
    set_tts_settings: () => undefined,
    preview_voice: () => undefined,
    get_effective_hotkey: () => ({
      accelerator: "CommandOrControl+Shift+R",
      source: "default",
    }),
    set_hotkey: () => undefined,
  });
}

const continueBtn = () => screen.getByRole("button", { name: /Continue|Working…/ });
const skipBtn = () => screen.getByRole("button", { name: /Skip for now|Skip and finish/ });

async function goToStep(step: 2 | 3 | 4, user: ReturnType<typeof userEvent.setup>) {
  await screen.findByText(/Step 1 of 4/);
  await user.click(continueBtn()); // step 1 has no commit
  await screen.findByText(/Step 2 of 4/);
  if (step === 2) return;
  await user.click(skipBtn());
  await screen.findByText(/Step 3 of 4/);
  if (step === 3) return;
  await user.click(skipBtn());
  await screen.findByText(/Step 4 of 4/);
}

describe("Onboarding wizard", () => {
  beforeEach(() => resetTauriMocks());

  it("advances with Continue and Skip, goes Back, and finishes from the last step", async () => {
    mockWizardWorld();
    const onFinish = vi.fn();
    const user = userEvent.setup();
    render(<Onboarding onFinish={onFinish} />);

    expect(await screen.findByText(/Step 1 of 4: Permissions/)).toBeTruthy();
    await user.click(continueBtn());
    expect(await screen.findByText(/Step 2 of 4: Connect your AI/)).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByText(/Step 1 of 4/)).toBeTruthy();

    await user.click(continueBtn());
    await screen.findByText(/Step 2 of 4/);
    await user.click(skipBtn());
    expect(await screen.findByText(/Step 3 of 4: Voice/)).toBeTruthy();
    await user.click(skipBtn());
    expect(await screen.findByText(/Step 4 of 4: Hotkeys/)).toBeTruthy();

    expect(onFinish).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Finish" }));
    expect(onFinish).toHaveBeenCalledTimes(1);
  });

  it("skipping never writes bindings and never blocks Finish", async () => {
    mockWizardWorld();
    const onFinish = vi.fn();
    const user = userEvent.setup();
    render(<Onboarding onFinish={onFinish} />);

    await screen.findByText(/Step 1 of 4/);
    await user.click(continueBtn());
    await screen.findByText(/Step 2 of 4/);
    await user.click(skipBtn());
    await screen.findByText(/Step 3 of 4/);
    await user.click(skipBtn());
    await screen.findByText(/Step 4 of 4/);
    await user.click(screen.getByRole("button", { name: "Skip and finish" }));

    expect(onFinish).toHaveBeenCalledTimes(1);
    expect(invokeCalls("set_binding")).toHaveLength(0);
  });

  it("renders permission rows from statuses and refreshes on window focus", async () => {
    mockWizardWorld();
    render(<Onboarding onFinish={() => {}} />);

    expect(await screen.findByText("Granted ✓")).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Grant" })).toHaveLength(2);
    expect(screen.getByRole("button", { name: "Open System Settings" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Refresh" })).toBeNull();

    const before = invokeCalls("get_all_permission_statuses").length;
    fireEvent.focus(window);
    await waitFor(() =>
      expect(invokeCalls("get_all_permission_statuses").length).toBe(before + 1),
    );
  });

  it("saves the OpenAI key and writes the three default bindings on Continue", async () => {
    mockWizardWorld({ hasKey: false });
    const user = userEvent.setup();
    render(<Onboarding onFinish={() => {}} />);
    await goToStep(2, user);

    await user.type(screen.getByLabelText("OpenAI API key"), "sk-test-123");
    await user.click(continueBtn());
    await screen.findByText(/Step 3 of 4/);

    expect(invokeCalls("set_credential")).toEqual([
      { providerRef: "openai", secret: "sk-test-123" },
    ]);
    expect(invokeCalls("set_binding")).toEqual([
      {
        feature: "default",
        slot: "llm",
        engine: "openai",
        model: "gpt-4o-mini",
        providerRef: "openai",
      },
      {
        feature: "default",
        slot: "stt",
        engine: "openai-stt",
        model: "whisper-1",
        providerRef: "openai",
      },
      {
        feature: "default",
        slot: "tts",
        engine: "openai-tts",
        model: "tts-1",
        providerRef: "openai",
      },
    ]);
    // Alloy becomes the active voice, mirroring the defaults picker. The
    // fallback model stays empty: it is only ever a local sherpa id, and the
    // cloud model lives on the binding that was just written.
    expect(invokeCalls("set_tts_settings")).toEqual([
      { settings: { active_voice: "alloy", active_model: null } },
    ]);
  });

  it("does not overwrite an existing default binding on re-run", async () => {
    mockWizardWorld({
      existingBindings: {
        "default/llm": { engine_id: "openai-compatible", model: null, provider_ref: "groq" },
      },
    });
    const user = userEvent.setup();
    render(<Onboarding onFinish={() => {}} />);
    await goToStep(2, user);

    await user.click(continueBtn());
    await screen.findByText(/Step 3 of 4/);

    const slots = invokeCalls("set_binding").map((args) => args?.slot);
    expect(slots).toEqual(["stt", "tts"]);
  });

  it("writes no bindings when OpenAI is chosen without a key", async () => {
    mockWizardWorld({ hasKey: false });
    const user = userEvent.setup();
    render(<Onboarding onFinish={() => {}} />);
    await goToStep(2, user);

    await user.click(continueBtn());
    await screen.findByText(/Step 3 of 4/);

    expect(invokeCalls("set_credential")).toHaveLength(0);
    expect(invokeCalls("set_binding")).toHaveLength(0);
  });

  it("keeps previously configured voice defaults when re-running the wizard", async () => {
    mockWizardWorld({
      hasKey: true,
      existingBindings: {
        "default/stt": { engine_id: "parakeet", model: "parakeet-v3", provider_ref: null },
        "default/tts": { engine_id: "sherpa-tts", model: "vits-piper-amy", provider_ref: null },
      },
    });
    const user = userEvent.setup();
    render(<Onboarding onFinish={() => {}} />);
    await goToStep(3, user);

    // Existing bindings win over the recommended/cloud preselection.
    const local = (await screen.findByRole("radio", {
      name: /Parakeet v3/,
    })) as HTMLInputElement;
    expect(local.checked).toBe(true);
    const ttsLocal = screen.getByRole("radio", {
      name: /A voice on this Mac/,
    }) as HTMLInputElement;
    expect(ttsLocal.checked).toBe(true);

    await user.click(continueBtn());
    await screen.findByText(/Step 4 of 4/);

    expect(invokeCalls("set_binding")).toEqual([
      {
        feature: "default",
        slot: "stt",
        engine: "parakeet",
        model: "parakeet-v3",
        providerRef: null,
      },
      {
        feature: "default",
        slot: "tts",
        engine: "sherpa-tts",
        model: "vits-piper-amy",
        providerRef: null,
      },
    ]);
  });

  it("downloads the local speech model inline, then activates it on completion", async () => {
    mockWizardWorld({ hasKey: true });
    const user = userEvent.setup();
    render(<Onboarding onFinish={() => {}} />);
    await goToStep(3, user);

    // Key present and step 2 skipped → cloud is preselected; choose local.
    const local = await screen.findByRole("radio", { name: /Whisper Base/ });
    const sttGroup = screen.getByRole("radiogroup", { name: "Speech to text" });
    expect(
      (within(sttGroup).getByRole("radio", { name: /OpenAI cloud/ }) as HTMLInputElement).checked,
    ).toBe(true);
    await user.click(local);

    await waitFor(() => expect(invokeCalls("download_whisper_model")).toHaveLength(1));
    expect(invokeCalls("download_whisper_model")[0]).toEqual({ modelId: "whisper-base" });
    expect(invokeCalls("set_binding")).toHaveLength(0);

    act(() => {
      emitTauriEvent("model:download:progress", {
        model_id: "whisper-base",
        bytes_received: 74 * MB,
        bytes_total: 148 * MB,
      });
    });
    expect(await screen.findByText(/Downloading 50%/)).toBeTruthy();

    act(() => {
      emitTauriEvent("model:download:complete", { model_id: "whisper-base" });
    });

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toEqual({
      feature: "default",
      slot: "stt",
      engine: "whisper",
      model: "whisper-base",
      providerRef: null,
    });
    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_dictation_settings")[0]).toEqual({
      settings: { post_process: false, active_model: "whisper-base" },
    });
  });

  it("previews the selected cloud voice", async () => {
    mockWizardWorld({ hasKey: true });
    const user = userEvent.setup();
    render(<Onboarding onFinish={() => {}} />);
    await goToStep(3, user);

    const voiceSelect = await screen.findByRole("combobox", { name: "Voice" });
    await user.selectOptions(voiceSelect, "nova");
    await user.click(screen.getByRole("button", { name: "Preview voice" }));

    await waitFor(() => expect(invokeCalls("preview_voice")).toHaveLength(1));
    expect(invokeCalls("preview_voice")[0]).toEqual({
      engine: "openai-tts",
      model: null,
      voice: "nova",
    });
  });
});
