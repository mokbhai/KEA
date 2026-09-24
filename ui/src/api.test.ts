import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  addMeetingActionItem,
  discoverLocalLlms,
  exportMeetingMarkdown,
  getPermissionStatus,
  meetingMarkdown,
  onApiOpenPage,
  onAssistantAnswer,
  onAssistantPartial,
  onAssistantState,
  onMeetingNotes,
  onMeetingNotesError,
  requestPermission,
  revealPath,
  setMeetingActionItemStatus,
  setMeetingTitle,
  type AssistantAnswer,
  type AssistantStatus,
  type MeetingNotes,
} from "./api";
import {
  emitTauriEvent,
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "./test-utils/tauri";

vi.mock("@tauri-apps/api/core", async () => (await import("./test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("./test-utils/tauri")).eventModule);

/**
 * The bindings for plan items 16–18, tested at the boundary that actually
 * breaks: the command name and the argument keys.
 *
 * Tauri matches arguments by name and silently passes `undefined` for a key
 * the frontend spelled differently, so a renamed parameter fails at runtime
 * with a type error from the backend rather than at compile time here.
 */
describe("meeting commands", () => {
  beforeEach(() => {
    resetTauriMocks();
    onInvoke({
      meeting_markdown: () => "# Weekly Sync\n",
      export_meeting_markdown: () => "/Users/x/Downloads/weekly-sync-2026-09-19.md",
      set_meeting_action_item_status: () => undefined,
      add_meeting_action_item: () => undefined,
      set_meeting_title: () => undefined,
      open_path_in_file_manager: () => undefined,
      get_permission_status: () => "Granted",
      request_permission: () => "Denied",
    });
  });

  it("asks for the Markdown of one meeting", async () => {
    expect(await meetingMarkdown("m1")).toBe("# Weekly Sync\n");
    expect(invokeCalls("meeting_markdown")).toEqual([{ meetingId: "m1" }]);
  });

  it("returns the path the export wrote, for the reveal that follows", async () => {
    const path = await exportMeetingMarkdown("m1");
    expect(path).toBe("/Users/x/Downloads/weekly-sync-2026-09-19.md");
    expect(invokeCalls("export_meeting_markdown")).toEqual([{ meetingId: "m1" }]);

    await revealPath(path);
    expect(invokeCalls("open_path_in_file_manager")).toEqual([{ path }]);
  });

  it("ticks an action item off by row id, not by text", async () => {
    await setMeetingActionItemStatus(7, "done");
    expect(invokeCalls("set_meeting_action_item_status")).toEqual([
      { id: 7, status: "done" },
    ]);
  });

  it("adds an action item to a meeting", async () => {
    await addMeetingActionItem("m1", "send the deck");
    expect(invokeCalls("add_meeting_action_item")).toEqual([
      { meetingId: "m1", text: "send the deck" },
    ]);
  });

  it("renames a meeting", async () => {
    await setMeetingTitle("m1", "Budget review");
    expect(invokeCalls("set_meeting_title")).toEqual([
      { meetingId: "m1", title: "Budget review" },
    ]);
  });

  // The kind is the string `PERM_KINDS` in src-tauri/src/commands.rs spells.
  // Anything else is rejected there with "unknown permission kind".
  it("carries the calendar permission kind through both commands", async () => {
    expect(await getPermissionStatus("calendar")).toBe("Granted");
    expect(await requestPermission("calendar")).toBe("Denied");
    expect(invokeCalls("get_permission_status")).toEqual([{ kind: "calendar" }]);
    expect(invokeCalls("request_permission")).toEqual([{ kind: "calendar" }]);
  });

  // Apple's recognizer needs this one, and requesting it is what pops the
  // system dialog — the kind has to be spelled as PERM_KINDS spells it or the
  // Grant button reports "unknown permission kind" instead.
  it("carries the speech permission kind through both commands", async () => {
    expect(await getPermissionStatus("speech")).toBe("Granted");
    expect(await requestPermission("speech")).toBe("Denied");
    expect(invokeCalls("get_permission_status")).toEqual([{ kind: "speech" }]);
    expect(invokeCalls("request_permission")).toEqual([{ kind: "speech" }]);
  });
});

describe("local LLM discovery", () => {
  beforeEach(() => resetTauriMocks());

  /** No arguments, and an empty list is a normal answer rather than a failure. */
  it("asks the backend to probe and passes the servers through", async () => {
    const servers = [
      {
        id: "ollama",
        display_name: "Ollama",
        base_url: "http://127.0.0.1:11434/v1",
        models: ["qwen3:8b"],
      },
    ];
    onInvoke({ discover_local_llms: () => servers });
    expect(await discoverLocalLlms()).toEqual(servers);
    expect(invokeCalls("discover_local_llms")).toEqual([undefined]);
  });

  it("reports finding nothing as an empty list", async () => {
    onInvoke({ discover_local_llms: () => [] });
    expect(await discoverLocalLlms()).toEqual([]);
  });
});

describe("meeting note events", () => {
  beforeEach(resetTauriMocks);

  const notes: MeetingNotes = {
    meeting_id: "m1",
    summary: "we talked",
    decisions: "ship it",
    action_items: "- send the deck",
    follow_ups: "",
    open_questions: "",
    prompt_version: "interim-v1",
    engine_id: "openai",
    model: "gpt-4o-mini",
  };

  it("hands the interim notes row straight to the handler", async () => {
    const handler = vi.fn();
    await onMeetingNotes(handler);
    emitTauriEvent("meeting:notes", notes);
    expect(handler).toHaveBeenCalledWith(notes);
  });

  // A failed interim pass is not a failed meeting, so it rides its own event
  // and unwraps to the same `{ message }` shape `meeting:error` uses.
  it("unwraps a notes failure to its message", async () => {
    const handler = vi.fn();
    await onMeetingNotesError(handler);
    emitTauriEvent("meeting:notes_error", { message: "provider refused" });
    expect(handler).toHaveBeenCalledWith("provider refused");
  });

  it("delivers the page name a kea://open URL carried", async () => {
    const handler = vi.fn();
    await onApiOpenPage(handler);
    emitTauriEvent("api:open-page", "meetings");
    expect(handler).toHaveBeenCalledWith("meetings");
  });
});

/**
 * The assistant listeners, tested at the only thing that can go wrong quietly:
 * the event name.
 *
 * `assistant:state`, `assistant:answer` and `assistant:partial` are string
 * literals repeated on both sides of the Tauri boundary — `emit_assistant_state`,
 * `emit_assistant_answer` and `emit_assistant_partial` in
 * `src-tauri/src/events.rs` spell them, and `listen` here spells them again.
 * Nothing links the two, so a typo on either side is not a compile error and not
 * a runtime error: the backend emits into the void, the surface never redraws,
 * and the session looks like a shortcut that did nothing. These tests are the
 * only place that comparison is made.
 *
 * Each test emits before asserting anything about unsubscription, so a listener
 * that never subscribed fails here rather than passing an unlisten check that
 * would be vacuously true.
 */
describe("assistant events", () => {
  beforeEach(resetTauriMocks);

  it("delivers a state change on the event name the backend emits", async () => {
    const handler = vi.fn();
    await onAssistantState(handler);
    const status: AssistantStatus = { state: "listening" };
    emitTauriEvent("assistant:state", status);
    expect(handler).toHaveBeenCalledWith(status);
  });

  /**
   * `message` and `speaking` are absent on the states they do not describe
   * (`skip_serializing_if` on the Rust payload), so the surface distinguishes
   * the states by the fields it receives. The listener must not normalise them
   * into a fixed shape — a `speaking: false` invented for a failed state would
   * read as "answering silently" instead of "it broke".
   */
  it("carries the per-state detail fields through untouched", async () => {
    const handler = vi.fn();
    await onAssistantState(handler);

    emitTauriEvent("assistant:state", { state: "failed", message: "no microphone" });
    expect(handler).toHaveBeenLastCalledWith({
      state: "failed",
      message: "no microphone",
    });

    emitTauriEvent("assistant:state", { state: "presenting", speaking: true });
    expect(handler).toHaveBeenLastCalledWith({ state: "presenting", speaking: true });
  });

  it("stops delivering state changes once unsubscribed", async () => {
    const handler = vi.fn();
    const unlisten = await onAssistantState(handler);

    emitTauriEvent("assistant:state", { state: "listening" });
    expect(handler).toHaveBeenCalledTimes(1);

    unlisten();
    emitTauriEvent("assistant:state", { state: "processing" });
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("delivers an answer on the event name the backend emits", async () => {
    const handler = vi.fn();
    await onAssistantAnswer(handler);
    const answer: AssistantAnswer = {
      request: "what is on my calendar",
      text: "Two meetings.",
      read: "Weekly Sync, Budget review",
      sent_externally: true,
    };
    emitTauriEvent("assistant:answer", answer);
    expect(handler).toHaveBeenCalledWith(answer);
  });

  /**
   * The listener forwards `event.payload` whole rather than rebuilding it key
   * by key. `speech_error` is the case that proves it matters: Rust puts it on
   * `AssistantAnswerPayload` and `AssistantAnswer` here does not declare it
   * yet, and a listener that copied the declared fields across would drop it
   * silently — the answer would appear with no sign that reading it aloud had
   * failed, which is the one thing the disclosure is for.
   */
  it("forwards answer fields the TypeScript type does not yet declare", async () => {
    const handler = vi.fn();
    await onAssistantAnswer(handler);
    const payload = {
      request: "read me the notes",
      text: "Here they are.",
      speech_error: "no voice installed",
    };
    emitTauriEvent("assistant:answer", payload);
    expect(handler).toHaveBeenCalledWith(payload);
  });

  it("stops delivering answers once unsubscribed", async () => {
    const handler = vi.fn();
    const unlisten = await onAssistantAnswer(handler);

    emitTauriEvent("assistant:answer", { request: "first", text: "one" });
    expect(handler).toHaveBeenCalledTimes(1);

    unlisten();
    emitTauriEvent("assistant:answer", { request: "second", text: "two" });
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("delivers a live hypothesis on the event name the backend emits", async () => {
    const handler = vi.fn();
    await onAssistantPartial(handler);

    emitTauriEvent("assistant:partial", { text: "what time" });

    expect(handler).toHaveBeenCalledWith({ text: "what time" });
  });

  /**
   * Hypotheses replace one another rather than accumulating, and each one is
   * the whole request so far. A listener that fired only on the first would
   * leave the first word frozen on screen for the rest of the question, which
   * looks exactly like recognition having died.
   */
  it("delivers every hypothesis, not just the first", async () => {
    const handler = vi.fn();
    await onAssistantPartial(handler);

    emitTauriEvent("assistant:partial", { text: "what" });
    emitTauriEvent("assistant:partial", { text: "what time" });
    emitTauriEvent("assistant:partial", { text: "what time is it" });

    expect(handler).toHaveBeenCalledTimes(3);
    expect(handler).toHaveBeenLastCalledWith({ text: "what time is it" });
  });

  it("stops delivering hypotheses once unsubscribed", async () => {
    const handler = vi.fn();
    const unlisten = await onAssistantPartial(handler);

    emitTauriEvent("assistant:partial", { text: "what" });
    expect(handler).toHaveBeenCalledTimes(1);

    unlisten();
    emitTauriEvent("assistant:partial", { text: "what time" });
    expect(handler).toHaveBeenCalledTimes(1);
  });

  /**
   * A copy-pasted listener that kept the name it was copied from would make
   * both surfaces redraw on every event. The pass-through tests above would
   * still catch a swap, but not a duplicate, because each of them only ever
   * emits its own event.
   */
  it("keeps the three events apart", async () => {
    const state = vi.fn();
    const answer = vi.fn();
    const partial = vi.fn();
    await onAssistantState(state);
    await onAssistantAnswer(answer);
    await onAssistantPartial(partial);

    emitTauriEvent("assistant:state", { state: "processing" });
    expect(answer).not.toHaveBeenCalled();
    expect(partial).not.toHaveBeenCalled();

    emitTauriEvent("assistant:answer", { request: "q", text: "a" });
    expect(state).toHaveBeenCalledTimes(1);
    expect(partial).not.toHaveBeenCalled();

    emitTauriEvent("assistant:partial", { text: "q" });
    expect(state).toHaveBeenCalledTimes(1);
    expect(answer).toHaveBeenCalledTimes(1);
  });
});
