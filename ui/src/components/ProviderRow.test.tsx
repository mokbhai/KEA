import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import ProviderRow from "./ProviderRow";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const OPENAI = { provider_ref: "openai", name: "OpenAI", built_in: true };
const CUSTOM = { provider_ref: "groq", name: "Groq", built_in: false };

function mockProviderWorld({ hasKey = false }: { hasKey?: boolean } = {}) {
  let saved = hasKey;
  onInvoke({
    has_credential: () => saved,
    get_provider_config: () => ({ base_url: "", default_model: "" }),
    set_credential: () => {
      saved = true;
    },
    delete_credential: () => {
      saved = false;
    },
    set_provider_config: () => undefined,
    test_provider: () => ({ ok: true, message: "Connected" }),
    remove_custom_provider: () => undefined,
  });
}

/** Opens the row's detail panel, where the key controls live. */
async function expand(name = "OpenAI") {
  await userEvent.click(await screen.findByRole("button", { name: "Edit" }));
  return name;
}

describe("ProviderRow", () => {
  beforeEach(() => resetTauriMocks());

  it("saves a pasted key from the button and reports it as saved", async () => {
    mockProviderWorld();
    render(<ProviderRow provider={OPENAI} onRemoved={() => {}} />);
    await expand();

    await userEvent.type(await screen.findByLabelText("OpenAI API key"), "  sk-test  ");
    await userEvent.click(screen.getByRole("button", { name: "Save key" }));

    await waitFor(() => expect(invokeCalls("set_credential")).toHaveLength(1));
    // Padding from a paste is trimmed off before it reaches the keychain.
    expect(invokeCalls("set_credential")[0]).toEqual({
      providerRef: "openai",
      secret: "sk-test",
    });
    expect(await screen.findByText("Saved ✓")).toBeTruthy();
    expect(screen.getByLabelText("API key saved")).toBeTruthy();
  });

  it("commits the key on Enter", async () => {
    mockProviderWorld();
    render(<ProviderRow provider={OPENAI} onRemoved={() => {}} />);
    await expand();

    await userEvent.type(await screen.findByLabelText("OpenAI API key"), "sk-typed{Enter}");

    await waitFor(() => expect(invokeCalls("set_credential")).toHaveLength(1));
    expect(invokeCalls("set_credential")[0]).toEqual({
      providerRef: "openai",
      secret: "sk-typed",
    });
    expect(await screen.findByText("Saved ✓")).toBeTruthy();
  });

  it("does not save an empty key", async () => {
    mockProviderWorld();
    render(<ProviderRow provider={OPENAI} onRemoved={() => {}} />);
    await expand();

    const input = await screen.findByLabelText("OpenAI API key");
    await userEvent.type(input, "   {Enter}");

    expect(invokeCalls("set_credential")).toHaveLength(0);
    expect(screen.getByRole("button", { name: "Save key" }).hasAttribute("disabled")).toBe(true);
  });

  it("replaces a saved key, and can back out of replacing", async () => {
    mockProviderWorld({ hasKey: true });
    render(<ProviderRow provider={OPENAI} onRemoved={() => {}} />);
    await expand();

    // A saved key is masked until the user asks to replace it.
    expect(await screen.findByLabelText("API key saved")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Replace" }));

    await userEvent.type(await screen.findByLabelText("OpenAI API key"), "sk-new");
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));

    // Cancelling drops the draft and restores the masked key.
    expect(invokeCalls("set_credential")).toHaveLength(0);
    expect(screen.getByLabelText("API key saved")).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: "Replace" }));
    await userEvent.type(await screen.findByLabelText("OpenAI API key"), "sk-new{Enter}");

    await waitFor(() => expect(invokeCalls("set_credential")).toHaveLength(1));
    expect(invokeCalls("set_credential")[0]).toEqual({
      providerRef: "openai",
      secret: "sk-new",
    });
    expect(await screen.findByLabelText("API key saved")).toBeTruthy();
  });

  it("removes a saved key and returns to the entry field", async () => {
    mockProviderWorld({ hasKey: true });
    render(<ProviderRow provider={OPENAI} onRemoved={() => {}} />);
    await expand();

    await userEvent.click(await screen.findByRole("button", { name: "Remove" }));

    await waitFor(() => expect(invokeCalls("delete_credential")).toHaveLength(1));
    expect(invokeCalls("delete_credential")[0]).toEqual({ providerRef: "openai" });
    expect(await screen.findByLabelText("OpenAI API key")).toBeTruthy();
    expect(screen.queryByLabelText("API key saved")).toBeNull();
  });

  it("surfaces a keychain failure instead of claiming the key was saved", async () => {
    onInvoke({
      has_credential: () => false,
      get_provider_config: () => null,
      set_credential: () => {
        throw new Error("keychain locked");
      },
    });
    render(<ProviderRow provider={OPENAI} onRemoved={() => {}} />);
    await expand();

    await userEvent.type(await screen.findByLabelText("OpenAI API key"), "sk-test{Enter}");

    expect(await screen.findByText(/keychain locked/)).toBeTruthy();
    expect(screen.queryByText("Saved ✓")).toBeNull();
  });

  it("removes a custom provider only after confirmation", async () => {
    mockProviderWorld();
    const onRemoved = vi.fn();
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    render(<ProviderRow provider={CUSTOM} onRemoved={onRemoved} />);
    await userEvent.click(await screen.findByRole("button", { name: "Edit" }));

    await userEvent.click(screen.getByRole("button", { name: "Remove provider" }));
    expect(invokeCalls("remove_custom_provider")).toHaveLength(0);
    expect(onRemoved).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await userEvent.click(screen.getByRole("button", { name: "Remove provider" }));

    await waitFor(() => expect(invokeCalls("remove_custom_provider")).toHaveLength(1));
    expect(invokeCalls("remove_custom_provider")[0]).toEqual({ providerRef: "groq" });
    expect(onRemoved).toHaveBeenCalled();
    confirm.mockRestore();
  });

  it("offers no removal for a built-in provider", async () => {
    mockProviderWorld();
    render(<ProviderRow provider={OPENAI} onRemoved={() => {}} />);
    await expand();

    expect(screen.queryByRole("button", { name: "Remove provider" })).toBeNull();
  });
});
