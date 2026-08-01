import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  emitTauriEvent,
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import DefaultsPicker from "./DefaultsPicker";

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
    render(<DefaultsPicker capability="stt" open onClose={() => {}} onApplied={() => {}} />);

    expect(await screen.findByText("installed ✓")).toBeTruthy();
    expect(screen.getByText("key ✓")).toBeTruthy();
  });

  it("shows the download size for missing models and disables keyless cloud options", async () => {
    mockSttWorld({ installed: [], hasKey: false });
    render(<DefaultsPicker capability="stt" open onClose={() => {}} onApplied={() => {}} />);

    expect(await screen.findByText("148.0 MB ⬇")).toBeTruthy();
    expect(screen.getByText("Key missing")).toBeTruthy();
    const cloud = screen.getByRole("button", { name: /OpenAI whisper-1/ });
    expect(cloud.hasAttribute("disabled")).toBe(true);
  });

  it("downloads a missing local model, then activates it on completion", async () => {
    mockSttWorld({ installed: [] });
    const onApplied = vi.fn();
    render(<DefaultsPicker capability="stt" open onClose={() => {}} onApplied={onApplied} />);

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

  it("writes a feature-scoped override when given a feature and slot", async () => {
    mockSttWorld({ installed: ["whisper-base"] });
    render(
      <DefaultsPicker
        capability="stt"
        feature="meetings"
        slot="stt"
        open
        onClose={() => {}}
        onApplied={() => {}}
      />,
    );

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
    render(
      <DefaultsPicker
        capability="stt"
        feature="dictation"
        slot="stt"
        open
        onClose={() => {}}
        onApplied={() => {}}
      />,
    );

    await userEvent.click(await screen.findByRole("button", { name: /Whisper Base/ }));

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toMatchObject({
      feature: "dictation",
      slot: "stt",
    });
  });
});
