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

  it("titles the page with the only h1", async () => {
    mockWorld();
    render(<RewritePage />);

    expect(
      await screen.findByRole("heading", { level: 1, name: "Rewrite" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

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
      // Both engines registered: the override picks the local server, whose
      // engine is "openai-compatible".
      engines: { llm: ["openai", "openai-compatible"] },
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
      // Both engines registered: the override picks the local server, whose
      // engine is "openai-compatible".
      engines: { llm: ["openai", "openai-compatible"] },
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

  it("rewrites with the style just chosen in the form", async () => {
    // The page and the form read one state: a style picked here reaches the
    // run without a second copy being pushed back up.
    mockWorld({
      engines: { llm: ["openai"] },
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
      extra: { preview_rewrite: () => "Ship it Friday." },
    });
    render(<RewritePage />);

    await userEvent.selectOptions(
      await screen.findByLabelText("Rewrite style"),
      "concise",
    );
    await userEvent.click(screen.getByRole("button", { name: "Rewrite this" }));

    await waitFor(() => expect(invokeCalls("preview_rewrite")).toHaveLength(1));
    expect(invokeCalls("preview_rewrite")[0]).toMatchObject({ mode: "concise" });
  });

  it("sends the target language as the run's parameter when translating", async () => {
    mockWorld({
      engines: { llm: ["openai"] },
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
      extra: {
        get_setting: (args) =>
          ({
            "rewrite.active_mode": "translate",
            "rewrite.translate.target": "de",
          })[args?.key as string] ?? null,
        preview_rewrite: () => "Guten Tag.",
      },
    });
    render(<RewritePage />);

    await userEvent.click(await screen.findByRole("button", { name: "Rewrite this" }));

    await waitFor(() => expect(invokeCalls("preview_rewrite")).toHaveLength(1));
    // One argument carries whatever the mode needs, so translate's tag rides
    // in the slot Ask KEA's instruction uses.
    expect(invokeCalls("preview_rewrite")[0]).toMatchObject({
      mode: "translate",
      customInstruction: "de",
    });
  });

  it("gives every enabled language its own shortcut row", async () => {
    mockWorld({
      engines: { llm: ["openai"] },
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
      extra: {
        get_setting: (args) =>
          args?.key === "rewrite.translate.targets" ? '["fr","ja"]' : null,
      },
    });
    render(<RewritePage />);

    const shortcuts = await screen.findByRole("group", { name: "Translate shortcuts" });
    expect(shortcuts.textContent).toContain("French");
    expect(shortcuts.textContent).toContain("Japanese");
  });

  it("adds a language to the shortcut list", async () => {
    mockWorld({
      engines: { llm: ["openai"] },
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
      extra: {
        get_setting: (args) =>
          args?.key === "rewrite.translate.targets" ? '["fr"]' : null,
      },
    });
    render(<RewritePage />);

    await userEvent.selectOptions(
      await screen.findByLabelText("Language to add"),
      "de",
    );
    await userEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() =>
      expect(
        invokeCalls("set_setting").filter(
          (c) => c?.key === "rewrite.translate.targets",
        ),
      ).toHaveLength(1),
    );
    expect(
      invokeCalls("set_setting").find((c) => c?.key === "rewrite.translate.targets"),
    ).toEqual({ key: "rewrite.translate.targets", value: '["fr","de"]' });
  });

  it("removes a language from the shortcut list", async () => {
    mockWorld({
      engines: { llm: ["openai"] },
      bindings: { "default/llm": openAiBinding("openai", "gpt-4o-mini") },
      extra: {
        get_setting: (args) =>
          args?.key === "rewrite.translate.targets" ? '["fr","ja"]' : null,
      },
    });
    render(<RewritePage />);

    await userEvent.click(await screen.findByRole("button", { name: "Remove French" }));

    await waitFor(() =>
      expect(
        invokeCalls("set_setting").find((c) => c?.key === "rewrite.translate.targets"),
      ).toEqual({ key: "rewrite.translate.targets", value: '["ja"]' }),
    );
    // And the shortcut goes with it: a combo still registered for a language
    // the page no longer lists is a key stolen from every other app with no
    // way left to get it back.
    expect(invokeCalls("clear_hotkey")).toEqual([
      { feature: "rewrite", command: "translate.fr" },
    ]);
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
