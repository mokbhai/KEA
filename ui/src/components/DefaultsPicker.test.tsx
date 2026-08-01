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

  it("clears a stale dictation model when the pick is not a whisper one", async () => {
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

    // Cleared, not replaced: dictation.active_model is the fallback for
    // whichever engine a model-less binding resolves to, and parakeet ids
    // live in a different namespace from whisper's.
    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_dictation_settings")[0]).toEqual({
      settings: { post_process: true, active_model: null },
    });
  });

  it("never leaves a dictation model that a local engine could not load", async () => {
    // The break this guards: local onboarding leaves default stt =
    // whisper/null; the user sets a dictation override to OpenAI whisper-1;
    // "use default again" deletes the override; resolve falls back to
    // whisper/null, takes the model from active_model and whisper fails with
    // "model not installed: whisper-1". So a cloud pick must clear the
    // fallback rather than mirror its own model id into it.
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

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    const written = invokeCalls("set_dictation_settings")[0] as {
      settings: { active_model: string | null };
    };
    expect(written.settings.active_model).toBeNull();
    expect(written.settings.active_model).not.toBe("whisper-1");
    // The binding itself still carries the cloud model.
    expect(invokeCalls("set_binding")[0]).toMatchObject({
      engine: "openai-stt",
      model: "whisper-1",
    });
  });

  it("clears the local voice model when picking cloud voices", async () => {
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
    expect(invokeCalls("set_tts_settings")[0]).toEqual({
      settings: { active_voice: "alloy", active_model: null },
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
