import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import { featureHandlers, openAiBinding } from "../test-utils/featureWorld";
import RewritePage from "./RewritePage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const settingsHandlers = {
  get_setting: () => null,
  set_setting: () => undefined,
  list_presets: () => [],
  get_prompt_override: () => null,
};

function mockWorld(options: Parameters<typeof featureHandlers>[0] = {}) {
  onInvoke(
    featureHandlers({
      ...options,
      extra: { ...settingsHandlers, ...(options.extra ?? {}) },
    }),
  );
}

describe("RewritePage", () => {
  beforeEach(() => resetTauriMocks());

  it("warns when nothing is set up and offers a way to choose", async () => {
    // Two engines and no binding: the backend would ask for a choice.
    mockWorld({ engines: { llm: ["openai", "openai-compatible"] } });
    render(<RewritePage />);

    expect(await screen.findByText(/Nothing is set up for this yet/)).toBeTruthy();
    // One in the banner, one in the AI card row.
    expect(screen.getAllByRole("button", { name: "Choose…" })).toHaveLength(2);
  });

  it("warns when the resolved provider has no API key and links the fix", async () => {
    mockWorld({
      engines: { llm: ["openai"] },
      hasKey: false,
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
    });
    const onNavigate = vi.fn();
    render(<RewritePage onNavigate={onNavigate} />);

    expect(await screen.findByText(/OpenAI needs an API key/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Open AI Providers" }));
    expect(onNavigate).toHaveBeenCalledWith("ai-providers");
  });

  it("shows no banner once a default resolves", async () => {
    mockWorld({
      engines: { llm: ["openai"] },
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
    });
    render(<RewritePage />);

    expect(
      await screen.findByText("Using default — OpenAI · gpt-4o-mini"),
    ).toBeTruthy();
    expect(screen.queryByText(/Nothing is set up for this yet/)).toBeNull();
    expect(screen.queryByText(/needs an API key/)).toBeNull();
  });

  it("writes a rewrite-scoped override from the AI card", async () => {
    mockWorld({
      engines: { llm: ["openai"] },
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
    });
    render(<RewritePage />);

    await userEvent.click(await screen.findByRole("button", { name: "Change…" }));
    await userEvent.click(await screen.findByRole("button", { name: /Local server/ }));

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toEqual({
      feature: "rewrite",
      slot: "llm",
      engine: "openai-compatible",
      model: null,
      providerRef: "local-llm",
    });
  });

  it("drops the override when asked to use the default again", async () => {
    mockWorld({
      engines: { llm: ["openai"] },
      bindings: {
        "default/llm": openAiBinding("openai", "gpt-4o-mini"),
        "rewrite/llm": openAiBinding("openai", "gpt-4o"),
      },
    });
    render(<RewritePage />);

    expect(await screen.findByText("This feature only — OpenAI · gpt-4o")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Use default again" }));

    await waitFor(() => expect(invokeCalls("delete_binding")).toHaveLength(1));
    expect(invokeCalls("delete_binding")[0]).toEqual({
      feature: "rewrite",
      slot: "llm",
    });
  });

  it("rewrites the sample text without pasting it anywhere", async () => {
    mockWorld({
      engines: { llm: ["openai"] },
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
      extra: { preview_rewrite: () => "I think we should ship this on Friday." },
    });
    render(<RewritePage />);

    await userEvent.click(await screen.findByRole("button", { name: "Rewrite this" }));

    expect(
      await screen.findByText("I think we should ship this on Friday."),
    ).toBeTruthy();
    expect(invokeCalls("trigger_rewrite")).toHaveLength(0);
    expect(invokeCalls("preview_rewrite")[0]).toMatchObject({
      mode: "improve",
      presetId: null,
      customInstruction: null,
    });
  });
});
