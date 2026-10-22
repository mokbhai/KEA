import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MeetingChatMessage, ProposalMeta } from "../../api";
import { invokeCalls, onInvoke, resetTauriMocks } from "../../test-utils/tauri";
import { meetingDetail, meetingNotes } from "../../test-utils/meetingFixtures";
import AskAiTab, { splitCitations, type AskAiTabProps } from "./AskAiTab";

vi.mock("@tauri-apps/api/core", async () => (await import("../../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../../test-utils/tauri")).eventModule);

beforeEach(() => {
  resetTauriMocks();
});

const CREATED = "2026-09-19T10:20:00Z";

function message(over: Partial<MeetingChatMessage> & Pick<MeetingChatMessage, "id" | "role" | "content">): MeetingChatMessage {
  return { proposal: null, created_at: CREATED, ...over };
}

function proposal(over: Partial<ProposalMeta> = {}): ProposalMeta {
  return {
    proposal: {
      summary: "Launch slips a week.",
      decisions: "",
      action_items: "",
      follow_ups: "",
      open_questions: "",
    },
    state: "pending",
    changed: ["summary"],
    ...over,
  };
}

/** A deferred reply, so a test can hold a turn open and then let it land. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function renderAsk(over: Partial<AskAiTabProps> = {}) {
  const props: AskAiTabProps = {
    meetingId: "m1",
    recording: false,
    durationMs: 1_800_000,
    notes: meetingNotes,
    onJumpTo: vi.fn(),
    onNotesChanged: vi.fn(),
    aiReady: true,
    ...over,
  };
  const utils = render(<AskAiTab {...props} />);
  return { ...utils, props };
}

const input = () => screen.findByLabelText("Ask about this meeting") as Promise<HTMLTextAreaElement>;

describe("splitCitations", () => {
  it("splits a reply into text and [mm:ss] citations, in order", () => {
    expect(splitCitations("At [01:05] Dan spoke, then [12:40].")).toEqual([
      { kind: "text", text: "At " },
      { kind: "cite", label: "[01:05]", ms: 65_000 },
      { kind: "text", text: " Dan spoke, then " },
      { kind: "cite", label: "[12:40]", ms: 760_000 },
      { kind: "text", text: "." },
    ]);
  });

  it("leaves text without citations, and seconds past 59, alone", () => {
    expect(splitCitations("no times here")).toEqual([{ kind: "text", text: "no times here" }]);
    expect(splitCitations("not [01:75] a time")).toEqual([{ kind: "text", text: "not [01:75] a time" }]);
  });
});

describe("AskAiTab history and sending", () => {
  it("loads the saved chat on mount and shows user and assistant messages", async () => {
    onInvoke({
      get_meeting_chat: () => [
        message({ id: 1, role: "user", content: "What did Dan say?" }),
        message({ id: 2, role: "assistant", content: "He asked about CI/CD." }),
      ],
    });
    renderAsk();
    expect(await screen.findByText("What did Dan say?")).toBeTruthy();
    expect(screen.getByText("He asked about CI/CD.")).toBeTruthy();
    expect(invokeCalls("get_meeting_chat")).toEqual([{ meetingId: "m1" }]);
  });

  it("sends on Enter, and Shift+Enter makes a new line instead", async () => {
    const send = vi.fn(() => ({ message: message({ id: 2, role: "assistant", content: "ok" }), truncated: false }));
    onInvoke({ get_meeting_chat: () => [], send_meeting_chat: send });
    renderAsk();
    const box = await input();

    await userEvent.type(box, "line one{Shift>}{Enter}{/Shift}line two");
    expect(box.value).toBe("line one\nline two");
    expect(invokeCalls("send_meeting_chat")).toHaveLength(0);

    await userEvent.type(box, "{Enter}");
    await waitFor(() => expect(invokeCalls("send_meeting_chat")).toHaveLength(1));
    expect(invokeCalls("send_meeting_chat")[0]).toEqual({ meetingId: "m1", message: "line one\nline two" });
  });

  it("shows a pending bubble and disables Send while the reply is being written", async () => {
    const reply = deferred<unknown>();
    let history: MeetingChatMessage[] = [];
    onInvoke({
      get_meeting_chat: () => history,
      send_meeting_chat: () => reply.promise,
    });
    renderAsk();
    const box = await input();
    await userEvent.type(box, "Summarise the blockers");
    await userEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(screen.getByRole("status").textContent).toBe("Thinking…");
    expect(screen.getByRole("button", { name: "Send" }).hasAttribute("disabled")).toBe(true);
    expect(box.disabled).toBe(true);

    history = [
      message({ id: 1, role: "user", content: "Summarise the blockers" }),
      message({ id: 2, role: "assistant", content: "Only the vendor delay." }),
    ];
    reply.resolve({ message: history[1], truncated: false });

    expect(await screen.findByText("Only the vendor delay.")).toBeTruthy();
    expect(screen.queryByRole("status")).toBeNull();
    expect(box.value).toBe("");
  });

  it("keeps the typed text and shows the error when the send fails", async () => {
    onInvoke({
      get_meeting_chat: () => [],
      send_meeting_chat: () => Promise.reject("a reply is still being written"),
    });
    renderAsk();
    const box = await input();
    await userEvent.type(box, "still there?");
    await userEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("a reply is still being written");
    expect(box.value).toBe("still there?");
  });

  it("shows the truncated notice once a reply reports a long transcript", async () => {
    onInvoke({
      get_meeting_chat: () => [],
      send_meeting_chat: () => ({
        message: message({ id: 2, role: "assistant", content: "Partly." }),
        truncated: true,
      }),
    });
    renderAsk();
    const box = await input();
    await userEvent.type(box, "anything?");
    await userEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("Answers are based on the first part of a long transcript.")).toBeTruthy();
  });

  it("shows a button to the Models page instead of an input when no LLM is bound", async () => {
    const onOpenModels = vi.fn();
    onInvoke({ get_meeting_chat: () => [] });
    renderAsk({ aiReady: false, aiBlockedReason: "Choose a model for Notes writing.", onOpenModels });

    expect(screen.queryByLabelText("Ask about this meeting")).toBeNull();
    expect(screen.getByText("Choose a model for Notes writing.")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Open Models" }));
    expect(onOpenModels).toHaveBeenCalledTimes(1);
  });

  it("clears the chat only after confirmation", async () => {
    onInvoke({
      get_meeting_chat: () => [message({ id: 1, role: "user", content: "hello there" })],
      clear_meeting_chat: () => undefined,
    });
    renderAsk();
    await screen.findByText("hello there");

    await userEvent.click(screen.getByRole("button", { name: "Clear chat" }));
    await userEvent.click(screen.getByRole("button", { name: "Keep it" }));
    expect(invokeCalls("clear_meeting_chat")).toHaveLength(0);
    expect(screen.getByText("hello there")).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: "Clear chat" }));
    await userEvent.click(screen.getByRole("button", { name: "Yes, clear chat" }));
    expect(invokeCalls("clear_meeting_chat")).toEqual([{ meetingId: "m1" }]);
    await waitFor(() => expect(screen.queryByText("hello there")).toBeNull());
  });
});

describe("AskAiTab quick prompts", () => {
  it("sends a quick prompt as a normal message", async () => {
    onInvoke({
      get_meeting_chat: () => [],
      send_meeting_chat: () => ({ message: message({ id: 2, role: "assistant", content: "Done." }), truncated: false }),
    });
    renderAsk();
    await userEvent.click(await screen.findByRole("button", { name: "Shorter" }));
    await waitFor(() => expect(invokeCalls("send_meeting_chat")).toHaveLength(1));
    expect(invokeCalls("send_meeting_chat")[0]).toEqual({
      meetingId: "m1",
      message: "Make the notes shorter.",
    });
  });

  it("asks for a language before a translation is sent", async () => {
    onInvoke({
      get_meeting_chat: () => [],
      send_meeting_chat: () => ({ message: message({ id: 2, role: "assistant", content: "Hola." }), truncated: false }),
    });
    renderAsk();
    await userEvent.click(await screen.findByRole("button", { name: "Translate" }));
    expect(invokeCalls("send_meeting_chat")).toHaveLength(0);

    await userEvent.type(screen.getByLabelText("Translate into which language"), "Spanish");
    await userEvent.click(screen.getByRole("button", { name: "Send translation" }));

    await waitFor(() => expect(invokeCalls("send_meeting_chat")).toHaveLength(1));
    expect(invokeCalls("send_meeting_chat")[0]).toEqual({
      meetingId: "m1",
      message: "Translate the notes into Spanish.",
    });
  });
});

describe("AskAiTab citations", () => {
  it("makes an in-range time a button that jumps, and leaves a later one as text", async () => {
    const onJumpTo = vi.fn();
    onInvoke({
      get_meeting_chat: () => [
        message({ id: 2, role: "assistant", content: "Raised at [00:07], again at [45:00]." }),
      ],
    });
    renderAsk({ onJumpTo });

    await userEvent.click(await screen.findByRole("button", { name: "[00:07]" }));
    expect(onJumpTo).toHaveBeenCalledWith(7_000);
    expect(screen.queryByRole("button", { name: "[45:00]" })).toBeNull();
    expect(screen.getByText("[45:00]").tagName).toBe("SPAN");
  });

  it("renders the reply as plain text, never as markup", async () => {
    onInvoke({
      get_meeting_chat: () => [
        message({ id: 2, role: "assistant", content: "<b>bold</b> and a second line\nhere" }),
      ],
    });
    const { container } = renderAsk();
    expect(await screen.findByText(/<b>bold<\/b> and a second line/)).toBeTruthy();
    expect(container.querySelector("b")).toBeNull();
  });
});

describe("AskAiTab notes proposals", () => {
  const proposalReply = (over: Partial<ProposalMeta> = {}) =>
    message({
      id: 2,
      role: "assistant",
      content: "Here is a shorter summary.",
      proposal: proposal(over),
    });

  it("shows before and after for each changed section, and changes nothing until Apply", async () => {
    const onNotesChanged = vi.fn();
    const apply = vi.fn(() => meetingDetail({ notes: { ...meetingNotes, summary: "Launch slips a week.", edited: true } }));
    onInvoke({ get_meeting_chat: () => [proposalReply()], apply_notes_proposal: apply });
    renderAsk({ onNotesChanged });

    const card = await screen.findByRole("group", { name: "Proposed notes change" });
    expect(card.textContent).toContain("Summary");
    expect(card.textContent).toContain("The launch slips a week.");
    expect(card.textContent).toContain("Launch slips a week.");
    expect(apply).not.toHaveBeenCalled();
    expect(onNotesChanged).not.toHaveBeenCalled();
  });

  it("applies once when Apply is clicked twice in a row, and passes the fresh detail up", async () => {
    const applied = meetingDetail({ notes: { ...meetingNotes, summary: "Launch slips a week.", edited: true } });
    const onNotesChanged = vi.fn();
    const apply = vi.fn(() => applied);
    onInvoke({
      get_meeting_chat: () => [proposalReply()],
      apply_notes_proposal: apply,
    });
    renderAsk({ onNotesChanged });

    const button = await screen.findByRole("button", { name: "Apply" });
    fireEvent.click(button);
    fireEvent.click(button);

    await waitFor(() => expect(onNotesChanged).toHaveBeenCalledTimes(1));
    expect(invokeCalls("apply_notes_proposal")).toEqual([{ meetingId: "m1", messageId: 2 }]);
    expect(onNotesChanged).toHaveBeenCalledWith(applied);
  });

  it("discards a proposal, then shows it as discarded with no buttons", async () => {
    let state: ProposalMeta["state"] = "pending";
    const discard = vi.fn(() => {
      state = "discarded";
      return undefined;
    });
    onInvoke({
      get_meeting_chat: () => [proposalReply({ state })],
      discard_notes_proposal: discard,
    });
    const onNotesChanged = vi.fn();
    renderAsk({ onNotesChanged });

    await userEvent.click(await screen.findByRole("button", { name: "Discard" }));

    expect(invokeCalls("discard_notes_proposal")).toEqual([{ meetingId: "m1", messageId: 2 }]);
    expect(await screen.findByText("Discarded.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Apply" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Discard" })).toBeNull();
    expect(onNotesChanged).not.toHaveBeenCalled();
  });

  it("shows an applied proposal as a label, with no buttons", async () => {
    onInvoke({ get_meeting_chat: () => [proposalReply({ state: "applied" })] });
    renderAsk();
    expect(await screen.findByText("Applied to the notes.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Apply" })).toBeNull();
  });

  it("offers no proposal controls while the meeting records, and says editing waits", async () => {
    onInvoke({ get_meeting_chat: () => [proposalReply()] });
    renderAsk({ recording: true });

    await screen.findByText("Here is a shorter summary.");
    expect(screen.queryByRole("button", { name: "Apply" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Discard" })).toBeNull();
    expect(screen.getByText("Editing is available when the meeting ends.")).toBeTruthy();
    // Questions are still allowed while recording.
    expect((await input()).disabled).toBe(false);
  });
});

describe("AskAiTab meeting switches", () => {
  it("drops a reply that lands after the user has moved to another meeting", async () => {
    const reply = deferred<unknown>();
    onInvoke({
      get_meeting_chat: (args) =>
        (args as { meetingId: string }).meetingId === "m1"
          ? []
          : [message({ id: 9, role: "assistant", content: "Meeting two notes." })],
      send_meeting_chat: () => reply.promise,
    });
    const { rerender, props } = renderAsk({ meetingId: "m1" });
    const box = await input();
    await userEvent.type(box, "question for one");
    await userEvent.click(screen.getByRole("button", { name: "Send" }));

    rerender(<AskAiTab {...props} meetingId="m2" />);
    expect(await screen.findByText("Meeting two notes.")).toBeTruthy();

    reply.resolve({ message: message({ id: 2, role: "assistant", content: "Late answer for one." }), truncated: true });
    await waitFor(() => expect(screen.queryByText("Late answer for one.")).toBeNull());
    expect(screen.queryByText("Answers are based on the first part of a long transcript.")).toBeNull();
  });
});
