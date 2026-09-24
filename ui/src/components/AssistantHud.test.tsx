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
  await answerPayloadArrives(ANSWER, speaking);
}

/** The same, for a turn whose payload is not the plain answered one. */
async function answerPayloadArrives(
  payload: Record<string, unknown>,
  speaking: boolean,
) {
  await act(async () => {
    emitTauriEvent("assistant:answer", payload);
  });
  await setState({ state: "presenting", speaking });
}

const answerText = () => document.querySelector(".kea-hud__answer")?.textContent ?? null;
const stopControl = () => screen.queryByText("Esc to stop speaking");
const copyControl = () => screen.queryByText("⌘⇧C to copy");
const heard = () => document.querySelector(".kea-hud__heard")?.textContent ?? null;

/** One live hypothesis arriving from the streaming recogniser. */
async function partialArrives(text: string) {
  await act(async () => {
    emitTauriEvent("assistant:partial", { text });
  });
}
const requestEcho = () => document.querySelector(".kea-hud__request")?.textContent ?? null;
const actionLabel = () => document.querySelector(".kea-hud__action")?.textContent ?? null;

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

  // The routing-visibility requirement: the user must be able to see the
  // request that was acted on *and* the action chosen for it. The two tests
  // below are a pair — either one alone would pass against a surface that
  // always showed the same thing.

  it("names no action for a request that was answered rather than acted on", async () => {
    await renderHud();

    await answerArrives(false);

    // The turn is on screen — so an absent label is a decision about this
    // payload, not a HUD that failed to render.
    expect(answerText()).toBe("Paris.");
    expect(actionLabel()).toBeNull();
  });

  it("names the action that ran, so an actioned turn does not read as an answered one", async () => {
    await renderHud();

    await answerPayloadArrives(
      {
        request: "open safari",
        action: "Open an application",
        text: "Opened Safari.",
      },
      false,
    );

    // The title the backend sent, not an id and not a re-derived phrase: the
    // frontend has no catalog to translate `open_app` with, which is why the
    // event carries the words the user reads.
    expect(actionLabel()).toBe("Open an application");
    expect(requestEcho()).toContain("open safari");
  });

  it("keeps naming the action that ran when showing the answer is off", async () => {
    // Hiding the reply is a preference about reading answers. Being told that
    // something was *done* is the visibility requirement, which no output
    // switch is allowed to turn off.
    await renderHud({ show_answers: false });

    await answerPayloadArrives(
      {
        request: "open safari",
        action: "Open an application",
        text: "Opened Safari.",
      },
      false,
    );

    expect(answerText()).toBeNull();
    expect(actionLabel()).toBe("Open an application");
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

  // Live recognition. The spec asks for the recognised text to appear *while*
  // the user is still speaking, and for its absence to be an ordinary session
  // rather than a stalled one — the streaming model is not shipped with the
  // app, so most first sessions have no partials at all.

  it("shows what it is hearing while the user is still speaking", async () => {
    await renderHud();

    await setState({ state: "listening" });
    await partialArrives("what time");

    expect(heard()).toBe("what time");
  });

  it("replaces each hypothesis with the next rather than accumulating them", async () => {
    // Every partial is the whole request so far. Appending them would render
    // "whatwhat timewhat time is it", which is the bug this asserts against.
    await renderHud();

    await setState({ state: "listening" });
    await partialArrives("what");
    await partialArrives("what time");
    await partialArrives("what time is it");

    expect(heard()).toBe("what time is it");
  });

  it("keeps the recognised text up while the request is being worked on", async () => {
    // `processing` is the window in which the user checks whether they were
    // heard correctly. Blanking the line on leaving `listening` would take the
    // evidence away at exactly the moment it is wanted.
    await renderHud();

    await setState({ state: "listening" });
    await partialArrives("what time is it");
    await setState({ state: "processing" });

    expect(heard()).toBe("what time is it");
  });

  it("says it is listening without a recognised line when live recognition is absent", async () => {
    // The ordinary case on a machine that never downloaded the streaming
    // model: the session still captures and still answers, and the surface
    // must not read as a stall.
    await renderHud();

    await setState({ state: "listening" });

    expect(heard()).toBeNull();
    expect(screen.getByText("Listening…")).toBeTruthy();
  });

  it("drops the hypothesis when the answer arrives, so one question is on screen at a time", async () => {
    // The answer carries the request as the offline transcript heard it. Two
    // versions of the same question side by side invites the user to wonder
    // which one was answered.
    await renderHud();

    await setState({ state: "listening" });
    await partialArrives("what is the capital of frans");
    await answerArrives(false);

    expect(heard()).toBeNull();
    expect(requestEcho()).toContain("what is the capital of France");
  });

  it("starts the next turn with no leftover hypothesis from the last one", async () => {
    await renderHud();

    await setState({ state: "listening" });
    await partialArrives("what time is it");
    await answerArrives(false);
    await setState({ state: "listening" });

    expect(heard()).toBeNull();
  });

  // The copy control. It is a key rather than a button because the overlay is
  // click-through and non-focusable by construction, so this line is the only
  // place the binding is discoverable — and the binding is registered around a
  // session, so a user who never reads it never finds out it exists.

  it("advertises the copy key while an answer is on screen", async () => {
    await renderHud();

    await answerArrives(false);

    expect(copyControl()).toBeTruthy();
  });

  it("does not advertise copying before there is an answer to copy", async () => {
    // The binding is held for the whole session, including the seconds before
    // the first answer, where pressing it deliberately does nothing. A hint
    // there would promise something the key does not do.
    await renderHud();

    await setState({ state: "listening" });

    expect(copyControl()).toBeNull();
  });

  it("does not advertise copying an answer it was told not to show", async () => {
    // With the display off there is nothing on screen to copy, and the user
    // asked for the answer to stay out of sight — offering to put it on the
    // clipboard would be the surface arguing with the setting.
    await renderHud({ show_answers: false });

    await answerArrives(true);

    expect(answerText()).toBeNull();
    expect(copyControl()).toBeNull();
  });

  it("offers copying and stopping together while an answer is still being read aloud", async () => {
    // Two different keys, so they do not compete for one slot: one silences
    // the voice, the other keeps the text.
    await renderHud({ speak_answers: true, show_answers: true });

    await answerArrives(true);

    expect(stopControl()).toBeTruthy();
    expect(copyControl()).toBeTruthy();
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
