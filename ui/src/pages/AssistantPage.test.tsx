import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import { featureHandlers, openAiBinding } from "../test-utils/featureWorld";
import type { AssistantSettings } from "../api";
import AssistantPage from "./AssistantPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

/** All three slots bound, which is the page in its ordinary state. */
const BOUND = {
  "assistant/stt": openAiBinding("openai-stt"),
  "assistant/llm": openAiBinding("openai"),
  "assistant/tts": openAiBinding("openai-tts"),
};

/**
 * Stands the page up with the two output switches in a given position;
 * omitting either gives the shipped default, which is on.
 */
function renderPage(
  outputs: Partial<AssistantSettings> = {},
  extra: Record<string, (args?: Record<string, unknown>) => unknown> = {},
) {
  onInvoke(
    featureHandlers({
      bindings: BOUND,
      extra: {
        get_assistant_settings: (): AssistantSettings => ({
          speak_answers: true,
          show_answers: true,
          ...outputs,
        }),
        set_assistant_settings: () => undefined,
        ...extra,
      },
    }),
  );
  return render(<AssistantPage />);
}

const switchNamed = (name: string) => screen.findByRole("switch", { name });

describe("AssistantPage", () => {
  beforeEach(() => resetTauriMocks());

  it("shows the shortcut row", async () => {
    renderPage();
    expect(await screen.findByText("Shortcut")).toBeTruthy();
  });

  it("advertises Escape, which is otherwise undiscoverable", async () => {
    // Escape is registered only while a session is open, so a user who never
    // reads this row has no way to learn the key does anything.
    renderPage();
    expect(await screen.findByText("Esc")).toBeTruthy();
  });

  it("lists what the assistant can do", async () => {
    renderPage();
    await waitFor(() => {
      expect(screen.getByText("Open an application")).toBeTruthy();
    });
    expect(screen.getByText("Read what's on screen")).toBeTruthy();
    expect(screen.getByText("Rewrite the selection")).toBeTruthy();
  });

  it("says plainly that nothing it does can destroy work", async () => {
    // The reason there is no confirmation flow anywhere in this feature, and
    // the thing a user needs to know before letting it read their screen.
    renderPage();
    expect(
      await screen.findByText(/changes or deletes your work/i),
    ).toBeTruthy();
  });

  it("offers both output switches, so neither channel is reachable only from a config file", async () => {
    renderPage();

    expect(await switchNamed("Speak answers")).toBeTruthy();
    expect(await switchNamed("Show answers")).toBeTruthy();
  });

  it("shows each switch where the user left it rather than where it ships", async () => {
    renderPage({ speak_answers: false, show_answers: true });

    const speak = await switchNamed("Speak answers");
    await waitFor(() => expect(speak.getAttribute("aria-checked")).toBe("false"));
    expect((await switchNamed("Show answers")).getAttribute("aria-checked")).toBe("true");
  });

  it("writes the switch that was flipped and carries the other one across", async () => {
    // Both switches go out in one payload, so a save that forgot to re-read
    // would turn speaking back on every time showing was turned off.
    renderPage({ speak_answers: false, show_answers: true });

    const show = await switchNamed("Show answers");
    await waitFor(() => expect(show.getAttribute("aria-checked")).toBe("true"));
    await userEvent.click(show);

    await waitFor(() => expect(invokeCalls("set_assistant_settings")).toHaveLength(1));
    const saved = invokeCalls("set_assistant_settings")[0] as {
      settings: AssistantSettings;
    };
    expect(saved.settings.show_answers).toBe(false);
    expect(saved.settings.speak_answers).toBe(false);
  });

  it("puts a switch back when the save is refused, so the page never claims a setting it does not have", async () => {
    renderPage(
      {},
      {
        set_assistant_settings: () => {
          throw new Error("disk is read-only");
        },
      },
    );

    const speak = await switchNamed("Speak answers");
    await waitFor(() => expect(speak.getAttribute("aria-checked")).toBe("true"));
    await userEvent.click(speak);

    expect(await screen.findByText("disk is read-only")).toBeTruthy();
    await waitFor(() => expect(speak.getAttribute("aria-checked")).toBe("true"));
  });

  it("warns when an answer would be neither spoken nor shown", async () => {
    // A legal combination — the actions still run — but one worth saying out
    // loud, since it otherwise looks like an assistant that stopped answering.
    renderPage({ speak_answers: false, show_answers: false });

    expect(await screen.findByText(/neither spoken nor shown/i)).toBeTruthy();
  });

  it("keeps quiet about the answer going nowhere while either channel is on", async () => {
    renderPage({ speak_answers: true, show_answers: false });

    await switchNamed("Show answers");
    expect(screen.queryByText(/neither spoken nor shown/i)).toBeNull();
  });
});
