import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { emitTauriEvent, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import type { AssistantSettings } from "../api";
import AssistantHud from "./AssistantHud";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

/** What one answered turn puts on the surface. */
const ANSWER = {
  request: "what is the capital of France",
  text: "Paris.",
};

/**
 * The settings the backend would hand back, with both switches on unless the
 * test is about one being off — the same default the Rust side reads for an
 * absent row.
 */
let stored: AssistantSettings = { speak_answers: true, show_answers: true };

/** Listeners are registered in an effect that awaits a promise, as is the
 *  settings read the surface gates the answer and the stop control on. */
async function renderHud(outputs: Partial<AssistantSettings> = {}) {
  stored = { speak_answers: true, show_answers: true, ...outputs };
  onInvoke({ get_assistant_settings: () => stored });
  const view = render(<AssistantHud />);
  await act(async () => {});
  return view;
}

async function setState(payload: {
  state: "listening" | "processing" | "presenting" | "failed";
  message?: string;
  speaking?: boolean;
}) {
  await act(async () => {
    emitTauriEvent("assistant:state", payload);
  });
}

/** An answer arriving, then the state that says whether it is being spoken. */
async function answerArrives(speaking: boolean) {
  await act(async () => {
    emitTauriEvent("assistant:answer", ANSWER);
  });
  await setState({ state: "presenting", speaking });
}

const answerText = () => document.querySelector(".kea-hud__answer")?.textContent ?? null;
const stopControl = () => screen.queryByText("Esc to stop speaking");
const requestEcho = () => document.querySelector(".kea-hud__request")?.textContent ?? null;

describe("AssistantHud", () => {
  beforeEach(() => resetTauriMocks());

  it("renders nothing until a session opens, so the overlay is empty at rest", async () => {
    const { container } = await renderHud();

    expect(container.firstChild).toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
  });

  // The four combinations of the two output switches. Each one is a separate
  // promise the spec makes to the user, and the pair is not symmetric: showing
  // is honoured here, speaking is honoured in the session before a sample is
  // ever synthesized, and this surface only agrees not to offer a control for
  // audio that will not play.

  it("shows the answer and offers a stop control when speaking and showing are both on", async () => {
    await renderHud({ speak_answers: true, show_answers: true });

    await answerArrives(true);

    expect(answerText()).toBe("Paris.");
    expect(stopControl()).toBeTruthy();
  });

  it("offers the stop control but no answer text when showing is off", async () => {
    // The hands-free case: the answer is in the air, so the only thing the
    // panel owes the user is a way to silence it.
    await renderHud({ speak_answers: true, show_answers: false });

    await answerArrives(true);

    expect(answerText()).toBeNull();
    expect(stopControl()).toBeTruthy();
  });

  it("shows the answer and offers no stop control when speaking is off", async () => {
    // Nothing is playing, so a stop control would be a button for silence that
    // is already there.
    await renderHud({ speak_answers: false, show_answers: true });

    await answerArrives(false);

    expect(answerText()).toBe("Paris.");
    expect(stopControl()).toBeNull();
  });

  it("presents the session without an answer or a stop control when both switches are off", async () => {
    await renderHud({ speak_answers: false, show_answers: false });

    await answerArrives(false);

    expect(answerText()).toBeNull();
    expect(stopControl()).toBeNull();
    // Still a session on screen: the user pressed a key and is owed the sight
    // of something happening, whatever they chose to do with the reply.
    expect(screen.getByRole("status")).toBeTruthy();
    expect(document.querySelector(".kea-hud__dot")).toBeTruthy();
  });

  it("keeps showing what it heard when showing the answer is off", async () => {
    // The request is not the answer. Turning the reply off is a choice about
    // reading, not a reason to hide a misheard question.
    await renderHud({ show_answers: false });

    await answerArrives(true);

    expect(requestEcho()).toContain("what is the capital of France");
  });

  it("keeps disclosing what was read and where it went when showing is off", async () => {
    // Being told that another app was read and that its text left the machine
    // is not something the display switch is allowed to buy silence on.
    await renderHud({ show_answers: false });

    await act(async () => {
      emitTauriEvent("assistant:answer", {
        ...ANSWER,
        read: "the front window",
        sent_externally: true,
      });
    });
    await setState({ state: "presenting", speaking: false });

    expect(screen.getByText(/Read the front window/)).toBeTruthy();
    expect(screen.getByText(/sent to your AI provider/)).toBeTruthy();
  });

  it("suppresses a stop control for a session that started before speaking was turned off", async () => {
    // The switches live in the settings window, which cannot reach this one.
    // Re-reading them as each turn begins is what keeps the surface from
    // offering to stop audio the user has since asked never to play.
    await renderHud({ speak_answers: true });

    await answerArrives(true);
    expect(stopControl()).toBeTruthy();

    stored = { speak_answers: false, show_answers: true };
    await setState({ state: "listening" });
    await answerArrives(true);

    expect(stopControl()).toBeNull();
  });

  it("advertises cancelling, not stopping, while it is still listening", async () => {
    await renderHud();

    await setState({ state: "listening" });

    expect(screen.getByText("Esc to cancel")).toBeTruthy();
    expect(stopControl()).toBeNull();
  });

  it("states why a request failed rather than leaving the pill blank", async () => {
    await renderHud();

    await setState({ state: "failed", message: "No microphone is available." });

    expect(screen.getByText("No microphone is available.")).toBeTruthy();
  });

  it("falls back to showing the answer when the switches cannot be read", async () => {
    // Losing the settings read must not lose the answer: the shipped default
    // is both channels on, and that is what an unanswered question resolves to.
    onInvoke({
      get_assistant_settings: () => {
        throw new Error("no config pool");
      },
    });
    render(<AssistantHud />);
    await act(async () => {});

    await answerArrives(true);

    expect(answerText()).toBe("Paris.");
    expect(stopControl()).toBeTruthy();
  });
});
