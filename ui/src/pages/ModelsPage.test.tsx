import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { emitTauriEvent, invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
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
    deprecated: false,
  },
];

/**
 * The streaming recogniser. It is downloaded and deleted like any other model
 * but binds to nothing — it is chosen by a setting — which is what the delete
 * test below is about.
 */
const STREAMING = {
  id: "streaming-zipformer-en-20m",
  display_name: "Streaming Zipformer 20M (English)",
  language: "en-US",
  url: "",
  size_bytes: 122 * MB,
  sha256: "",
  kind: "StreamingZipformer",
  deprecated: false,
};

/** A model that is still resolvable but is no longer worth downloading. */
const RETIRED = {
  id: "whisper-medium",
  display_name: "Whisper Medium",
  language: "en-US",
  url: "",
  size_bytes: 1530 * MB,
  sha256: "",
  deprecated: true,
};

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

  it("titles the page with the only h1", async () => {
    mockModelsWorld();
    render(<ModelsPage />);

    expect(await screen.findByRole("heading", { level: 1, name: "Models" })).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

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

  /// Retiring a model must not strand whoever already downloaded it: the row
  /// stays, with its Remove button, for exactly as long as the files are on
  /// disk. This is the difference between flagging an entry and deleting it.
  it("still lists a retired model that is installed", async () => {
    onInvoke({
      list_whisper_models: () => [...WHISPER_CATALOG, RETIRED],
      list_installed_whisper_models: () => ["whisper-medium"],
      list_onnx_models: () => [],
      list_installed_onnx_models: () => [],
      get_binding: () => null,
      delete_model: () => undefined,
    });
    render(<ModelsPage />);

    expect(await screen.findByText("Whisper Medium")).toBeTruthy();
    expect(screen.getByText(/no longer recommended/)).toBeTruthy();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await userEvent.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(invokeCalls("delete_model")).toHaveLength(1));
    expect(invokeCalls("delete_model")[0]).toEqual({
      kind: "whisper",
      modelId: "whisper-medium",
    });
  });

  /// ...and stops offering it to anyone who has not. A gigabyte and a half
  /// for a worse result than a smaller model is a bug in the catalog.
  it("does not offer a retired model that is not installed", async () => {
    onInvoke({
      list_whisper_models: () => [...WHISPER_CATALOG, RETIRED],
      list_installed_whisper_models: () => [],
      list_onnx_models: () => [],
      list_installed_onnx_models: () => [],
      get_binding: () => null,
    });
    render(<ModelsPage />);

    expect(await screen.findByText("Whisper Base")).toBeTruthy();
    expect(screen.queryByText("Whisper Medium")).toBeNull();
  });

  it("offers the streaming model in its own section", async () => {
    onInvoke({
      list_whisper_models: () => [],
      list_installed_whisper_models: () => [],
      list_onnx_models: (args) => (args?.kind === "streaming" ? [STREAMING] : []),
      list_installed_onnx_models: () => [],
      get_binding: () => null,
      download_onnx_model: () => undefined,
    });
    render(<ModelsPage />);

    expect(
      await screen.findByRole("heading", { level: 2, name: "Live preview — streaming" }),
    ).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Download" }));

    await waitFor(() => expect(invokeCalls("download_onnx_model")).toHaveLength(1));
    expect(invokeCalls("download_onnx_model")[0]).toEqual({
      kind: "streaming",
      modelId: "streaming-zipformer-en-20m",
    });
  });

  /**
   * Nothing binds to a streaming model, so removing one cannot unset the
   * speech-to-text default — and must not claim it will. Saying so would be
   * a lie the user then acts on.
   */
  it("does not claim a streaming model is the speech-to-text default", async () => {
    onInvoke({
      list_whisper_models: () => [],
      list_installed_whisper_models: () => [],
      list_onnx_models: (args) => (args?.kind === "streaming" ? [STREAMING] : []),
      list_installed_onnx_models: (args) =>
        args?.kind === "streaming" ? ["streaming-zipformer-en-20m"] : [],
      // The user does have a speech-to-text default; it just has nothing to
      // do with this model.
      get_binding: (args) =>
        args?.slot === "stt"
          ? { engine_id: "parakeet", model: "streaming-zipformer-en-20m", provider_ref: null }
          : null,
      delete_model: () => undefined,
    });
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<ModelsPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Remove" }));

    expect(confirmSpy.mock.calls[0][0]).toContain("Streaming Zipformer");
    expect(confirmSpy.mock.calls[0][0]).not.toContain("default");
    await waitFor(() => expect(invokeCalls("delete_model")).toHaveLength(1));
    expect(invokeCalls("delete_model")[0]).toEqual({
      kind: "streaming",
      modelId: "streaming-zipformer-en-20m",
    });
  });

  /** No streaming model in the catalog is not an error — it is the default. */
  it("says so plainly when a catalog is empty", async () => {
    mockModelsWorld();
    render(<ModelsPage />);

    await screen.findByRole("heading", { level: 2, name: "Live preview — streaming" });
    expect(screen.getAllByText("No models available.").length).toBeGreaterThan(0);
  });

  it("lets the user stop a download that has stopped moving", async () => {
    // A transfer that stalls holds the row at its last percentage with no way
    // out: clicking Download again is refused as "already in progress", so
    // without a cancel the only escape is restarting the app.
    onInvoke({
      list_whisper_models: () => WHISPER_CATALOG,
      list_installed_whisper_models: () => [],
      list_onnx_models: () => [],
      list_installed_onnx_models: () => [],
      get_binding: () => null,
      download_whisper_model: () => undefined,
      cancel_model_download: () => undefined,
    });
    render(<ModelsPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Download" }));
    await waitFor(() => expect(invokeCalls("download_whisper_model")).toHaveLength(1));

    act(() => {
      emitTauriEvent("model:download:progress", {
        model_id: "whisper-base",
        bytes_received: 3 * MB,
        bytes_total: 148 * MB,
      });
    });
    expect(await screen.findByText("2%")).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: /Cancel download/i }));

    await waitFor(() => expect(invokeCalls("cancel_model_download")).toHaveLength(1));
    expect(invokeCalls("cancel_model_download")[0]).toEqual({
      kind: "whisper",
      modelId: "whisper-base",
    });
  });
});
