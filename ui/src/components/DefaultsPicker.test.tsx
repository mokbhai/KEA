import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  emitTauriEvent,
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import { usePendingActivation } from "../hooks/usePendingActivation";
import DefaultsPicker, { type Capability } from "./DefaultsPicker";
import type { BindingTarget } from "../lib/capabilityDefaults";

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

/**
 * Stands in for a page: owns the pending activation and mounts/unmounts the
 * picker, so tests can close the popover the way a user does.
 */
function Harness({
  capability,
  target,
  onApplied,
}: {
  capability: Capability;
  target?: BindingTarget;
  onApplied?: () => void;
}) {
  const [open, setOpen] = useState(true);
  const activation = usePendingActivation(onApplied);
  return (
    <>
      {open && (
        <DefaultsPicker
          capability={capability}
          target={target}
          open
          onClose={() => setOpen(false)}
          activation={activation}
        />
      )}
      {!open && activation.pending && <span>still downloading</span>}
    </>
  );
}

function mockSttWorld({ installed = [] as string[], hasKey = true } = {}) {
  onInvoke({
    list_providers: () => [
      { provider_ref: "openai", name: "OpenAI", built_in: true },
      { provider_ref: "local-llm", name: "Local server", built_in: true },
    ],
    get_binding: () => null,
    has_credential: () => hasKey,
    list_whisper_models: () => WHISPER_CATALOG,
    list_installed_whisper_models: () => installed,
    list_onnx_models: () => [],
    list_installed_onnx_models: () => [],
    download_whisper_model: () => undefined,
    cancel_model_download: () => undefined,
    set_binding: () => undefined,
    get_dictation_settings: () => ({ post_process: false, active_model: null }),
    set_dictation_settings: () => undefined,
  });
}

describe("DefaultsPicker", () => {
  beforeEach(() => resetTauriMocks());

  it("shows installed and key readiness statuses", async () => {
    mockSttWorld({ installed: ["whisper-base"] });
    render(<Harness capability="stt" />);

    expect(await screen.findByText("installed ✓")).toBeTruthy();
    expect(screen.getByText("key ✓")).toBeTruthy();
  });

  it("shows the download size for missing models and disables keyless cloud options", async () => {
    mockSttWorld({ installed: [], hasKey: false });
    render(<Harness capability="stt" />);

    expect(await screen.findByText("148.0 MB ⬇")).toBeTruthy();
    expect(screen.getByText("Key missing")).toBeTruthy();
    const cloud = screen.getByRole("button", { name: /OpenAI whisper-1/ });
    expect(cloud.hasAttribute("disabled")).toBe(true);
  });

  it("downloads a missing local model, then activates it on completion", async () => {
    mockSttWorld({ installed: [] });
    const onApplied = vi.fn();
    render(<Harness capability="stt" onApplied={onApplied} />);

    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));

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
    expect(await screen.findByText("downloading 50%")).toBeTruthy();

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
    expect(onApplied).toHaveBeenCalled();
    expect(await screen.findByText("Saved ✓")).toBeTruthy();
  });

  it("still activates the choice when the picker is closed mid-download", async () => {
    mockSttWorld({ installed: [] });
    const onApplied = vi.fn();
    render(<Harness capability="stt" onApplied={onApplied} />);

    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));
    await waitFor(() => expect(invokeCalls("download_whisper_model")).toHaveLength(1));

    // The user closes the popover while the bytes are still coming down.
    await userEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByText("still downloading")).toBeTruthy();

    act(() => {
      emitTauriEvent("model:download:complete", { model_id: "whisper-base" });
    });

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toMatchObject({
      feature: "default",
      slot: "stt",
      model: "whisper-base",
    });
    expect(onApplied).toHaveBeenCalled();
  });

  it("clears the pending choice and shows the reason when a download fails", async () => {
    mockSttWorld({ installed: [] });
    render(<Harness capability="stt" />);

    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));
    await waitFor(() => expect(invokeCalls("download_whisper_model")).toHaveLength(1));

    act(() => {
      emitTauriEvent("model:download:error", {
        model_id: "whisper-base",
        message: "network unreachable",
      });
    });

    expect(await screen.findByText(/whisper-base: network unreachable/)).toBeTruthy();
    // No binding is written, and the option is selectable again.
    expect(invokeCalls("set_binding")).toHaveLength(0);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: /Whisper Base/ }).hasAttribute("disabled"),
      ).toBe(false),
    );
  });

  it("cancels a download the backend never reports on, and takes a new pick after", async () => {
    // The wedge this guards: a download task that dies without emitting
    // completion or error leaves the row on "starting download…" forever, and
    // `start` refuses to re-issue while a request is pending — so every later
    // click is swallowed and only restarting the app clears it. Cancel is the
    // way out, and it has to leave the picker usable.
    mockSttWorld({ installed: [] });
    render(<Harness capability="stt" />);

    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));
    await waitFor(() => expect(invokeCalls("download_whisper_model")).toHaveLength(1));
    expect(await screen.findByText("starting download…")).toBeTruthy();

    // No progress, completion or error event ever arrives.
    await userEvent.click(await screen.findByRole("button", { name: /Cancel download/i }));

    await waitFor(() => expect(invokeCalls("cancel_model_download")).toHaveLength(1));
    expect(invokeCalls("cancel_model_download")[0]).toEqual({
      kind: "whisper",
      modelId: "whisper-base",
    });
    await waitFor(() => expect(screen.queryByText("starting download…")).toBeNull());

    // The picker is live again: a second pick reaches the backend.
    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));
    await waitFor(() => expect(invokeCalls("download_whisper_model")).toHaveLength(2));
  });

  it("offers no cancel on a row that is not downloading", async () => {
    mockSttWorld({ installed: [] });
    render(<Harness capability="stt" />);

    await screen.findByRole("button", { name: /Whisper Base/ });
    expect(screen.queryByRole("button", { name: /Cancel download/i })).toBeNull();
  });

  it("surfaces a failed download start without leaving the picker stuck", async () => {
    mockSttWorld({ installed: [] });
    onInvoke({
      list_providers: () => [{ provider_ref: "openai", name: "OpenAI", built_in: true }],
      get_binding: () => null,
      has_credential: () => true,
      list_whisper_models: () => WHISPER_CATALOG,
      list_installed_whisper_models: () => [],
      list_onnx_models: () => [],
      list_installed_onnx_models: () => [],
      download_whisper_model: () => {
        throw new Error("already in progress");
      },
    });
    render(<Harness capability="stt" />);

    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));

    expect(await screen.findByText(/already in progress/)).toBeTruthy();
    expect(invokeCalls("set_binding")).toHaveLength(0);
  });

  it("lists one option per provider for the llm capability", async () => {
    onInvoke({
      list_providers: () => [
        { provider_ref: "openai", name: "OpenAI", built_in: true },
        { provider_ref: "local-llm", name: "Local server", built_in: true },
        { provider_ref: "groq", name: "Groq", built_in: false },
      ],
      get_binding: () => null,
      has_credential: (args) => (args as { providerRef: string }).providerRef === "openai",
      set_binding: () => undefined,
    });
    render(<Harness capability="llm" />);

    // Local servers need no key; a keyless cloud provider can't be picked.
    expect(await screen.findByRole("button", { name: /OpenAI/ })).toBeTruthy();
    expect(screen.getByText("No key needed")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Groq/ }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("button", { name: /OpenAI/ }).hasAttribute("disabled")).toBe(false);

    await userEvent.click(screen.getByRole("button", { name: /Local server/ }));

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toEqual({
      feature: "default",
      slot: "llm",
      engine: "openai-compatible",
      model: null,
      providerRef: "local-llm",
    });
    // Text has no per-feature model setting to keep in step.
    expect(invokeCalls("set_dictation_settings")).toHaveLength(0);
    expect(invokeCalls("set_tts_settings")).toHaveLength(0);
  });

  it("writes a feature-scoped override when given a feature and slot", async () => {
    mockSttWorld({ installed: ["whisper-base"] });
    render(<Harness capability="stt" target={{ feature: "meetings", slot: "stt" }} />);

    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toEqual({
      feature: "meetings",
      slot: "stt",
      engine: "whisper",
      model: "whisper-base",
      providerRef: null,
    });
    // The global dictation model belongs to dictation, not to a meetings
    // override — the override binding already carries the model.
    expect(invokeCalls("set_dictation_settings")).toHaveLength(0);
  });

  it("keeps the dictation model in step with a dictation override", async () => {
    mockSttWorld({ installed: ["whisper-base"] });
    render(<Harness capability="stt" target={{ feature: "dictation", slot: "stt" }} />);

    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toMatchObject({
      feature: "dictation",
      slot: "stt",
    });
  });

  it("leaves the dictation fallback alone when the pick is not a whisper one", async () => {
    onInvoke({
      list_providers: () => [{ provider_ref: "openai", name: "OpenAI", built_in: true }],
      get_binding: () => null,
      has_credential: () => true,
      list_whisper_models: () => [],
      list_installed_whisper_models: () => [],
      list_onnx_models: () => [
        {
          id: "parakeet-v2",
          display_name: "Parakeet v2",
          language: "English",
          url: "",
          size_bytes: 10 * MB,
          sha256: "",
        },
      ],
      list_installed_onnx_models: () => ["parakeet-v2"],
      set_binding: () => undefined,
      // A whisper model left over from an earlier pick.
      get_dictation_settings: () => ({ post_process: true, active_model: "whisper-base" }),
      set_dictation_settings: () => undefined,
    });
    render(<Harness capability="stt" />);

    await userEvent.click(await screen.findByRole("button", { name: /Parakeet v2/ }));

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    // Untouched, neither mirrored nor cleared. Mirroring would hand a
    // parakeet id (separate ONNX storage) to whisper; clearing would strip
    // the fallback a model-less whisper binding needs. The parakeet binding
    // carries its own model, so the retained whisper id is never read.
    expect(invokeCalls("set_dictation_settings")).toHaveLength(0);
  });

  it("never leaves a dictation model that a local engine could not load", async () => {
    // The break this guards: local onboarding leaves default stt =
    // whisper/null; the user sets a dictation override to OpenAI whisper-1;
    // "use default again" deletes the override; resolve falls back to
    // whisper/null, takes the model from active_model and whisper fails with
    // "model not installed: whisper-1". A cloud pick must not put its own
    // model id there.
    mockSttWorld({ installed: ["whisper-base"] });
    onInvoke({
      list_providers: () => [{ provider_ref: "openai", name: "OpenAI", built_in: true }],
      get_binding: () => null,
      has_credential: () => true,
      list_whisper_models: () => WHISPER_CATALOG,
      list_installed_whisper_models: () => ["whisper-base"],
      list_onnx_models: () => [],
      list_installed_onnx_models: () => [],
      set_binding: () => undefined,
      // Left over from an earlier local pick.
      get_dictation_settings: () => ({ post_process: false, active_model: "whisper-base" }),
      set_dictation_settings: () => undefined,
    });
    render(<Harness capability="stt" target={{ feature: "dictation", slot: "stt" }} />);

    await userEvent.click(await screen.findByRole("button", { name: /OpenAI whisper-1/ }));

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    // The binding carries the cloud model; the local fallback is not touched,
    // so it keeps the installed whisper id it already held.
    expect(invokeCalls("set_binding")[0]).toMatchObject({
      engine: "openai-stt",
      model: "whisper-1",
    });
    expect(invokeCalls("set_dictation_settings")).toHaveLength(0);
  });

  it("keeps a usable whisper fallback across a switch to another engine", async () => {
    // Steps 1-4 of the reachable break: (1) local onboarding leaves
    // default/stt = whisper/null; (2) a whisper pick on the dictation card
    // records its model as the fallback; (3) the user switches that override
    // to a cloud engine; (4) "use default again" drops the override and
    // resolve lands back on whisper/null — which transcribes only because
    // step 3 left the fallback in place.
    let dictation = { post_process: false, active_model: null as string | null };
    onInvoke({
      list_providers: () => [{ provider_ref: "openai", name: "OpenAI", built_in: true }],
      get_binding: () => null,
      has_credential: () => true,
      list_whisper_models: () => WHISPER_CATALOG,
      list_installed_whisper_models: () => ["whisper-base"],
      list_onnx_models: () => [],
      list_installed_onnx_models: () => [],
      set_binding: () => undefined,
      get_dictation_settings: () => dictation,
      set_dictation_settings: (args) => {
        dictation = (args as { settings: typeof dictation }).settings;
      },
    });
    render(<Harness capability="stt" target={{ feature: "dictation", slot: "stt" }} />);

    // Step 2: the whisper pick populates the fallback.
    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));
    await waitFor(() => expect(dictation.active_model).toBe("whisper-base"));

    // Step 3: switching to the cloud engine must not disturb it.
    await userEvent.click(screen.getByRole("button", { name: /OpenAI whisper-1/ }));
    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(2));
    expect(invokeCalls("set_binding")[1]).toMatchObject({ engine: "openai-stt" });

    // Step 4 works because this still names an installed whisper model.
    expect(dictation.active_model).toBe("whisper-base");
    expect(invokeCalls("set_dictation_settings")).toHaveLength(1);
  });

  it("sets the cloud voice without disturbing the local voice model", async () => {
    onInvoke({
      list_providers: () => [{ provider_ref: "openai", name: "OpenAI", built_in: true }],
      get_binding: () => null,
      has_credential: () => true,
      list_onnx_models: () => [],
      list_installed_onnx_models: () => [],
      set_binding: () => undefined,
      // A sherpa voice left over from an earlier pick.
      get_tts_settings: () => ({ active_voice: null, active_model: "vits-piper-en-us-amy-low" }),
      set_tts_settings: () => undefined,
    });
    render(<Harness capability="tts" />);

    // The play button next to the row is also named after the option.
    await userEvent.click(await screen.findByRole("button", { name: /^OpenAI voices/ }));

    await waitFor(() => expect(invokeCalls("set_tts_settings")).toHaveLength(1));
    // The voice is the cloud engine's; the model stays as the fallback for a
    // model-less sherpa binding, which would otherwise fall back to the
    // first catalog voice and may not have it installed.
    expect(invokeCalls("set_tts_settings")[0]).toEqual({
      settings: { active_voice: "alloy", active_model: "vits-piper-en-us-amy-low" },
    });
  });

  it("keeps the saved cloud voice when picking a local voice", async () => {
    // The voice dropdown always has a value, so a local pick must not send
    // it: that would silently reset a saved "nova" back to "alloy".
    onInvoke({
      list_providers: () => [{ provider_ref: "openai", name: "OpenAI", built_in: true }],
      get_binding: () => null,
      has_credential: () => true,
      list_onnx_models: () => [
        {
          id: "vits-piper-en-us-amy-low",
          display_name: "Amy",
          language: "English",
          url: "",
          size_bytes: 20 * MB,
          sha256: "",
        },
      ],
      list_installed_onnx_models: () => ["vits-piper-en-us-amy-low"],
      set_binding: () => undefined,
      get_tts_settings: () => ({ active_voice: "nova", active_model: null }),
      set_tts_settings: () => undefined,
    });
    render(<Harness capability="tts" />);

    await userEvent.click(await screen.findByRole("button", { name: /^Amy/ }));

    await waitFor(() => expect(invokeCalls("set_tts_settings")).toHaveLength(1));
    expect(invokeCalls("set_tts_settings")[0]).toEqual({
      settings: { active_voice: "nova", active_model: "vits-piper-en-us-amy-low" },
    });
  });
});
