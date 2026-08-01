import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
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

  it("saves the AI clean-up toggle", async () => {
    mockWorld({ bindings: { "default/stt": whisperBinding } });
    render(<DictationPage />);

    await userEvent.click(
      await screen.findByRole("switch", { name: "Clean up text with AI" }),
    );

    await waitFor(() => expect(invokeCalls("set_dictation_settings")).toHaveLength(1));
    expect(invokeCalls("set_dictation_settings")[0]).toEqual({
      settings: { post_process: true, active_model: null },
    });
  });
});
