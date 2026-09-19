import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  addMeetingActionItem,
  discoverLocalLlms,
  exportMeetingMarkdown,
  getPermissionStatus,
  meetingMarkdown,
  onApiOpenPage,
  onMeetingNotes,
  onMeetingNotesError,
  requestPermission,
  revealPath,
  setMeetingActionItemStatus,
  setMeetingTitle,
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
