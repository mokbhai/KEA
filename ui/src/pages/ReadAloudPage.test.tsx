import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import { featureHandlers, openAiBinding } from "../test-utils/featureWorld";
import ReadAloudPage from "./ReadAloudPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

function mockWorld(options: Parameters<typeof featureHandlers>[0] = {}) {
  onInvoke(
    featureHandlers({
      engines: { tts: ["openai-tts", "sherpa-tts"] },
      ...options,
      extra: { preview_voice: () => undefined, ...(options.extra ?? {}) },
    }),
  );
}

describe("ReadAloudPage", () => {
  beforeEach(() => resetTauriMocks());

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

  it("plays the sample through the resolved voice", async () => {
    mockWorld({ bindings: { "default/tts": openAiBinding("openai-tts") } });
    render(<ReadAloudPage />);

    await userEvent.click(await screen.findByRole("button", { name: /Play sample/ }));

    await waitFor(() => expect(invokeCalls("preview_voice")).toHaveLength(1));
    expect(invokeCalls("preview_voice")[0]).toEqual({
      engine: "openai-tts",
      model: null,
      voice: null,
    });
  });
});
