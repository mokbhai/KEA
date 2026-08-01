import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, invokeMock, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import AiProvidersPage from "./AiProvidersPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const BUILT_INS = [
  { provider_ref: "openai", name: "OpenAI", built_in: true },
  { provider_ref: "local-llm", name: "Local server", built_in: true },
];

function mockPageWorld({ providers = BUILT_INS } = {}) {
  onInvoke({
    list_providers: () => providers,
    get_binding: () => null,
    list_whisper_models: () => [],
    list_onnx_models: () => [],
    has_credential: () => false,
    get_provider_config: () => null,
    add_custom_provider: () => undefined,
    set_provider_config: () => undefined,
  });
}

/** Opens the inline "add provider" form. */
async function openAddForm() {
  await userEvent.click(await screen.findByRole("button", { name: /Add provider/ }));
}

describe("AiProvidersPage — adding a custom provider", () => {
  beforeEach(() => resetTauriMocks());

  it("derives the ref from the name and stores the URL after the provider exists", async () => {
    mockPageWorld();
    render(<AiProvidersPage />);
    await openAddForm();

    await userEvent.type(screen.getByLabelText("Provider name"), "My Server");
    await userEvent.type(screen.getByLabelText("Server URL"), "  http://localhost:8080/v1  ");
    await userEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => expect(invokeCalls("add_custom_provider")).toHaveLength(1));
    expect(invokeCalls("add_custom_provider")[0]).toEqual({
      providerRef: "my-server",
      name: "My Server",
    });
    expect(invokeCalls("set_provider_config")[0]).toEqual({
      providerRef: "my-server",
      config: { base_url: "http://localhost:8080/v1", default_model: "" },
    });

    // The config must not be written before the provider it belongs to.
    const order = invokeMock.mock.calls
      .map(([cmd]) => cmd)
      .filter((cmd) => cmd === "add_custom_provider" || cmd === "set_provider_config");
    expect(order).toEqual(["add_custom_provider", "set_provider_config"]);
  });

  it("skips the config write when no URL was given, and resets the form", async () => {
    mockPageWorld();
    render(<AiProvidersPage />);
    await waitFor(() => expect(invokeCalls("list_providers")).toHaveLength(1));
    await openAddForm();

    await userEvent.type(screen.getByLabelText("Provider name"), "Groq");
    await userEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => expect(invokeCalls("add_custom_provider")).toHaveLength(1));
    expect(invokeCalls("set_provider_config")).toHaveLength(0);

    // Form closes, and the list is re-read so the new row shows up.
    await waitFor(() => expect(screen.queryByLabelText("Provider name")).toBeNull());
    expect(invokeCalls("list_providers")).toHaveLength(2);

    // Reopening starts from an empty form rather than the previous draft.
    await openAddForm();
    expect((screen.getByLabelText("Provider name") as HTMLInputElement).value).toBe("");
    expect((screen.getByLabelText("Server URL") as HTMLInputElement).value).toBe("");
  });

  it("rejects a name that yields no ref", async () => {
    mockPageWorld();
    render(<AiProvidersPage />);
    await openAddForm();

    await userEvent.type(screen.getByLabelText("Provider name"), "!!!");
    await userEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(await screen.findByText(/at least one letter or number/)).toBeTruthy();
    expect(invokeCalls("add_custom_provider")).toHaveLength(0);
  });

  it("rejects a name that collides with an existing provider", async () => {
    mockPageWorld();
    render(<AiProvidersPage />);
    await openAddForm();

    await userEvent.type(screen.getByLabelText("Provider name"), "OpenAI");
    await userEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(await screen.findByText(/A provider "openai" already exists/)).toBeTruthy();
    expect(invokeCalls("add_custom_provider")).toHaveLength(0);
  });

  it("surfaces a backend rejection and keeps the draft", async () => {
    mockPageWorld();
    onInvoke({
      list_providers: () => BUILT_INS,
      get_binding: () => null,
      list_whisper_models: () => [],
      list_onnx_models: () => [],
      has_credential: () => false,
      get_provider_config: () => null,
      add_custom_provider: () => {
        throw new Error('Provider id "groq" may only use lowercase letters');
      },
    });
    render(<AiProvidersPage />);
    await openAddForm();

    await userEvent.type(screen.getByLabelText("Provider name"), "Groq");
    await userEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(await screen.findByText(/may only use lowercase letters/)).toBeTruthy();
    expect((screen.getByLabelText("Provider name") as HTMLInputElement).value).toBe("Groq");
  });
});
