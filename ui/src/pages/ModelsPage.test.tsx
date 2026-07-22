import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import ModelsPage from "./ModelsPage";

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

function mockModelsWorld() {
  onInvoke({
    list_whisper_models: () => WHISPER_CATALOG,
    list_installed_whisper_models: () => ["whisper-base"],
    list_onnx_models: () => [],
    list_installed_onnx_models: () => [],
    get_binding: (args) =>
      args?.slot === "stt"
        ? { engine_id: "whisper", model: "whisper-base", provider_ref: null }
        : null,
    delete_model: () => undefined,
  });
}

describe("ModelsPage", () => {
  beforeEach(() => resetTauriMocks());
  afterEach(() => vi.restoreAllMocks());

  it("confirms with the consequence before removing an active-default model", async () => {
    mockModelsWorld();
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<ModelsPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Remove" }));

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    expect(confirmSpy.mock.calls[0][0]).toContain("Whisper Base");
    expect(confirmSpy.mock.calls[0][0]).toContain("speech-to-text default");
    await waitFor(() => expect(invokeCalls("delete_model")).toHaveLength(1));
    expect(invokeCalls("delete_model")[0]).toEqual({
      kind: "whisper",
      modelId: "whisper-base",
    });
  });

  it("does not remove the model when the confirm dialog is cancelled", async () => {
    mockModelsWorld();
    vi.spyOn(window, "confirm").mockReturnValue(false);
    render(<ModelsPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Remove" }));

    expect(invokeCalls("delete_model")).toHaveLength(0);
  });
});
