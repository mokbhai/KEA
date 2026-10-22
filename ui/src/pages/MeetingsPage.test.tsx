import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  emitTauriEvent,
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import { featureHandlers, openAiBinding } from "../test-utils/featureWorld";
import MeetingsPage from "./MeetingsPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const whisperBinding = {
  engine_id: "whisper",
  model: "whisper-base",
  provider_ref: null,
};

const meetingHandlers = {
  list_meetings: () => [],
  get_meeting_state: () => ({ state: "idle", active_meeting_id: null }),
  get_meeting_settings: () => ({
    segment_duration_secs: 30,
    prefer_system_audio: true,
    interim_notes: false,
    interim_every_segments: 8,
    interim_every_minutes: 5,
    calendar_titles: false,
    voice_separation: false,
  }),
  set_meeting_settings: () => undefined,
  // Neither speaker model is installed, the default a fresh install is in.
  list_installed_onnx_models: () => [],
  download_onnx_model: () => undefined,
  // Not set up, which is the default a fresh install is in.
  get_notion_status: () => ({
    has_token: false,
    parent_page: "",
    parent_page_error: null,
  }),
  get_system_audio_capability: () => "mic_only",
  get_permission_status: () => "Granted",
  start_meeting: () => "meeting-1",
  stop_meeting: () => ({
    meeting: {
      id: "meeting-1",
      title: "Test capture",
      started_at: "2026-07-17T10:00:00Z",
      ended_at: "2026-07-17T10:00:10Z",
      status: "completed",
      capture_mode: "mic_only",
      stt_engine_id: "whisper",
      llm_engine_id: "openai",
      error: null,
      title_source: "llm",
    },
    segments: [],
    notes: null,
    speakers: [],
    action_items: [],
  }),
  get_meeting: () => ({
    meeting: {
      id: "meeting-1",
      title: "Test capture",
      started_at: "2026-07-17T10:00:00Z",
      ended_at: "2026-07-17T10:00:10Z",
      status: "completed",
      capture_mode: "mic_only",
      stt_engine_id: "whisper",
      llm_engine_id: "openai",
      error: null,
      title_source: "llm",
    },
    segments: [],
    notes: null,
    speakers: [],
    action_items: [],
  }),
};

const readyBindings = {
  "default/stt": whisperBinding,
  "default/llm": openAiBinding("openai", "gpt-4o-mini"),
};

function mockWorld(options: Parameters<typeof featureHandlers>[0] = {}) {
  onInvoke(
    featureHandlers({
      engines: { stt: ["whisper", "openai-stt"], llm: ["openai", "openai-compatible"] },
      ...options,
      extra: { ...meetingHandlers, ...(options.extra ?? {}) },
    }),
  );
}

const meetingRow = {
  id: "meeting-1",
  title: "Test capture",
  started_at: "2026-07-17T10:00:00Z",
  ended_at: "2026-07-17T10:00:10Z",
  status: "completed",
  capture_mode: "mic_only",
  stt_engine_id: "whisper",
  llm_engine_id: "openai",
  error: null,
  title_source: "llm",
};

const notionReady = {
  has_token: true,
  parent_page: "https://www.notion.so/Notes-0123456789abcdef0123456789abcdef",
  parent_page_error: null,
};

/// Settings live in the ⚙ slide-over, so a test that reaches a setting opens it
/// first, the way a user does.
async function openSettings() {
  await userEvent.click(await screen.findByRole("button", { name: "Meeting settings" }));
  return screen.findByRole("dialog", { name: "Meeting settings" });
}

describe("MeetingsPage", () => {
  beforeEach(() => resetTauriMocks());

  /// A button whose only behaviour is an error is worse than no button, and
  /// the fix for that error is elsewhere on this same page.
  it("hides the Notion export until Notion is set up", async () => {
    mockWorld({ bindings: readyBindings, extra: { list_meetings: () => [meetingRow] } });
    render(<MeetingsPage />);

    await userEvent.click(await screen.findByRole("option", { name: /Test capture/ }));
    await screen.findByRole("button", { name: "Copy Markdown" });
    expect(screen.queryByRole("button", { name: "Send to Notion" })).toBeNull();
  });

  it("sends a meeting to Notion and shows where it landed", async () => {
    mockWorld({
      bindings: readyBindings,
      extra: {
        list_meetings: () => [meetingRow],
        get_notion_status: () => notionReady,
        export_meeting_to_notion: () => "https://www.notion.so/Test-capture-abc",
      },
    });
    render(<MeetingsPage />);

    await userEvent.click(await screen.findByRole("option", { name: /Test capture/ }));
    await userEvent.click(await screen.findByRole("button", { name: "Send to Notion" }));

    await waitFor(() =>
      expect(invokeCalls("export_meeting_to_notion")).toEqual([{ meetingId: "meeting-1" }]),
    );
    // The link is what makes the export verifiable without leaving the app.
    expect(
      await screen.findByText(/Sent to Notion — https:\/\/www\.notion\.so\/Test-capture-abc/),
    ).toBeTruthy();
  });

  it("titles the page with the only h1", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    expect(
      await screen.findByRole("heading", { level: 1, name: "Meetings" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

  it("warns for each unconfigured slot", async () => {
    mockWorld({ bindings: {} });
    render(<MeetingsPage />);

    const banners = await screen.findAllByText(/Nothing is set up for this yet/);
    // One for speech to text, one for notes writing.
    expect(banners).toHaveLength(2);
    expect(screen.getByText(/Speech to text —/)).toBeTruthy();
    expect(screen.getByText(/Notes writing —/)).toBeTruthy();
  });

  it("warns when the meeting speech model is missing and links Models", async () => {
    mockWorld({ bindings: readyBindings, installedWhisper: [] });
    const onNavigate = vi.fn();
    render(<MeetingsPage onNavigate={onNavigate} />);

    expect(await screen.findByText(/Whisper Base isn't downloaded yet/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Open Models" }));
    expect(onNavigate).toHaveBeenCalledWith("models");
  });

  it("shows no banner once both slots resolve", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    expect(
      await screen.findByText("Using default — Whisper Base — on this Mac"),
    ).toBeTruthy();
    expect(screen.getByText("Using default — OpenAI · gpt-4o-mini")).toBeTruthy();
    expect(screen.queryByText(/Nothing is set up for this yet/)).toBeNull();
    expect(screen.queryByText(/isn't downloaded yet/)).toBeNull();
  });

  it("writes a meetings-scoped override for the notes slot", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    await openSettings();
    const changeButtons = await screen.findAllByRole("button", { name: "Change…" });
    // [0] speech to text, [1] notes writing.
    await userEvent.click(changeButtons[1]);
    await userEvent.click(await screen.findByRole("button", { name: /Local server/ }));

    await waitFor(() => expect(invokeCalls("set_binding")).toHaveLength(1));
    expect(invokeCalls("set_binding")[0]).toEqual({
      feature: "meetings",
      slot: "llm",
      engine: "openai-compatible",
      model: null,
      providerRef: "local-llm",
    });
  });

  it("drops a meetings override when asked to use the default again", async () => {
    mockWorld({
      bindings: { ...readyBindings, "meetings/stt": openAiBinding("openai-stt", "whisper-1") },
    });
    render(<MeetingsPage />);

    await openSettings();
    await userEvent.click(
      await screen.findByRole("button", { name: "Use default again" }),
    );

    await waitFor(() => expect(invokeCalls("delete_binding")).toHaveLength(1));
    expect(invokeCalls("delete_binding")[0]).toEqual({
      feature: "meetings",
      slot: "stt",
    });
  });

  it("does not judge meetings against dictation's fallback model", async () => {
    // meeting.rs passes binding.model straight through — it never falls back
    // to the dictation setting, so an undownloaded one must not block here.
    mockWorld({
      bindings: {
        ...readyBindings,
        "meetings/stt": { engine_id: "whisper", model: null, provider_ref: null },
      },
      installedWhisper: [],
      extra: {
        get_dictation_settings: () => ({
          post_process: false,
          active_model: "whisper-base",
        }),
      },
    });
    render(<MeetingsPage />);

    expect(await screen.findByText(/This feature only —/)).toBeTruthy();
    expect(screen.queryByText(/isn't downloaded yet/)).toBeNull();
  });

  it("puts a refused setting back instead of showing a value the backend rejected", async () => {
    mockWorld({
      bindings: readyBindings,
      extra: {
        set_meeting_settings: () => {
          throw new Error("disk is read-only");
        },
      },
    });
    render(<MeetingsPage />);

    await openSettings();
    const toggle = await screen.findByRole("switch", { name: "Record system audio too" });
    expect(toggle.getAttribute("aria-checked")).toBe("true");

    await userEvent.click(toggle);

    expect(await screen.findByText("disk is read-only")).toBeTruthy();
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("true"));
  });

  it("runs a labelled test capture that stops itself after ten seconds", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      mockWorld({ bindings: readyBindings });
      render(<MeetingsPage />);

      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      // The test lives with the other settings, so the panel opens first.
      await openSettings();
      await user.click(
        await screen.findByRole("button", { name: "Run a 10-second test" }),
      );

      await waitFor(() => expect(invokeCalls("start_meeting")).toHaveLength(1));
      expect(
        screen.getByText(/Test capture running — it stops itself in 10 seconds/),
      ).toBeTruthy();
      expect(invokeCalls("stop_meeting")).toHaveLength(0);

      // The backend confirms the capture is live, as it does on a real start.
      await act(async () => {
        emitTauriEvent("meeting:state", { state: "recording" });
      });

      await act(async () => {
        vi.advanceTimersByTime(10_000);
      });

      await waitFor(() => expect(invokeCalls("stop_meeting")).toHaveLength(1));
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the real error when a test capture fails after starting", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      mockWorld({ bindings: readyBindings });
      render(<MeetingsPage />);

      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      await openSettings();
      await user.click(
        await screen.findByRole("button", { name: "Run a 10-second test" }),
      );
      await waitFor(() => expect(invokeCalls("start_meeting")).toHaveLength(1));

      // start_meeting resolved, then capture failed and the backend went idle.
      await act(async () => {
        emitTauriEvent("meeting:error", { message: "microphone is unavailable" });
      });

      await act(async () => {
        vi.advanceTimersByTime(10_000);
      });

      // Stopping a meeting that isn't recording would only replace the cause
      // with "no meeting is recording".
      expect(invokeCalls("stop_meeting")).toHaveLength(0);
      expect(screen.getByText("microphone is unavailable")).toBeTruthy();
      // The test button is usable again rather than latched off.
      expect(
        screen.getByRole("button", { name: "Run a 10-second test" }).hasAttribute("disabled"),
      ).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("labels a live segment with the speaker the backend attributed it to", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);
    await screen.findByRole("heading", { level: 1, name: "Meetings" });

    await act(async () => {
      emitTauriEvent("meeting:state", { state: "recording" });
      emitTauriEvent("meeting:segment", {
        meeting_id: "meeting-1",
        sequence: 0,
        start_offset_ms: 0,
        end_offset_ms: 5000,
        text: "shall we start",
        speaker_key: "local",
      });
      emitTauriEvent("meeting:segment", {
        meeting_id: "meeting-1",
        sequence: 1,
        start_offset_ms: 5000,
        end_offset_ms: 9000,
        text: "yes go ahead",
        speaker_key: "remote",
      });
    });

    // Live lines are on the Transcript tab; Summary is the default.
    await userEvent.click(screen.getByRole("tab", { name: "Transcript" }));
    expect(await screen.findByText("shall we start")).toBeTruthy();
    expect(screen.getByText("You")).toBeTruthy();
    expect(screen.getByText("Others")).toBeTruthy();
  });

  // --- item 16a: notes while the meeting runs ---

  const interimNotes = {
    meeting_id: "meeting-1",
    summary: "The launch slips a week.",
    decisions: "",
    action_items: "Priya sends the deck",
    follow_ups: "",
    open_questions: "",
    prompt_version: "meeting-interim-v1",
    engine_id: "openai",
    model: "gpt-4o-mini",
  };

  it("shows interim notes as they arrive, labelled as unfinished", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);
    await screen.findByRole("heading", { level: 1, name: "Meetings" });

    // Nothing to show until a pass has run.
    expect(screen.queryByRole("heading", { name: "Notes so far" })).toBeNull();

    await act(async () => {
      emitTauriEvent("meeting:state", { state: "recording" });
      emitTauriEvent("meeting:notes", interimNotes);
    });

    expect(await screen.findByRole("heading", { name: "Notes so far" })).toBeTruthy();
    expect(screen.getByText("The launch slips a week.")).toBeTruthy();
    // A stale summary read as a final one is how this feature misleads.
    expect(
      screen.getByText(/The full notes are written when you stop/),
    ).toBeTruthy();
  });

  // Last meeting's notes are not this meeting's notes.
  it("clears interim notes when a new meeting starts recording", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);
    await screen.findByRole("heading", { level: 1, name: "Meetings" });

    await act(async () => {
      emitTauriEvent("meeting:state", { state: "recording" });
      emitTauriEvent("meeting:notes", interimNotes);
    });
    expect(await screen.findByRole("heading", { name: "Notes so far" })).toBeTruthy();

    await act(async () => {
      emitTauriEvent("meeting:state", { state: "idle" });
      emitTauriEvent("meeting:state", { state: "recording" });
    });
    await waitFor(() =>
      expect(screen.queryByRole("heading", { name: "Notes so far" })).toBeNull(),
    );
  });

  // The toggle spends the user's tokens on a schedule, so the cadence and the
  // cost are next to it rather than buried.
  it("offers interim notes off by default, with the cost spelled out", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    await openSettings();
    const toggle = await screen.findByRole("switch", {
      name: "Notes while the meeting runs",
    });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(screen.getByText(/AI calls an hour, billed to your notes provider/)).toBeTruthy();

    await userEvent.click(toggle);
    await waitFor(() => expect(invokeCalls("set_meeting_settings")).toHaveLength(1));
    const saved = invokeCalls("set_meeting_settings")[0] as {
      settings: Record<string, unknown>;
    };
    expect(saved.settings.interim_notes).toBe(true);
    // The fields this page does not know about must survive the write.
    expect(saved.settings.segment_duration_secs).toBe(30);
  });

  // --- speaker separation ---

  it("offers the speaker models instead of enabling voice separation without them", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    await openSettings();
    const toggle = await screen.findByRole("switch", {
      name: "Separate speakers by voice",
    });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    await userEvent.click(toggle);

    // Nothing is written: a setting that cannot run must not read as on.
    expect(invokeCalls("set_meeting_settings")).toHaveLength(0);
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    const download = await screen.findByRole("button", { name: "Download speaker models" });

    await userEvent.click(download);
    await waitFor(() => expect(invokeCalls("download_onnx_model")).toHaveLength(2));
    expect(invokeCalls("download_onnx_model")).toEqual([
      { kind: "diarization", modelId: "pyannote-segmentation-3-0" },
      { kind: "diarization", modelId: "campplus-sv-en-voxceleb-16k" },
    ]);
  });

  it("turns voice separation on once the downloads land", async () => {
    let installed: string[] = [];
    mockWorld({
      bindings: readyBindings,
      extra: { list_installed_onnx_models: () => installed },
    });
    render(<MeetingsPage />);

    await openSettings();
    await userEvent.click(
      await screen.findByRole("switch", { name: "Separate speakers by voice" }),
    );
    await userEvent.click(await screen.findByRole("button", { name: "Download speaker models" }));
    await waitFor(() => expect(invokeCalls("download_onnx_model")).toHaveLength(2));

    installed = ["pyannote-segmentation-3-0", "campplus-sv-en-voxceleb-16k"];
    await act(async () => {
      emitTauriEvent("model:download:complete", { model_id: "pyannote-segmentation-3-0" });
    });
    await act(async () => {
      emitTauriEvent("model:download:complete", { model_id: "campplus-sv-en-voxceleb-16k" });
    });

    await waitFor(() => expect(invokeCalls("set_meeting_settings")).toHaveLength(1));
    const saved = invokeCalls("set_meeting_settings")[0] as {
      settings: Record<string, unknown>;
    };
    expect(saved.settings.voice_separation).toBe(true);
  });

  it("enables voice separation directly when both models are already installed", async () => {
    mockWorld({
      bindings: readyBindings,
      extra: {
        list_installed_onnx_models: () => [
          "pyannote-segmentation-3-0",
          "campplus-sv-en-voxceleb-16k",
        ],
      },
    });
    render(<MeetingsPage />);

    await openSettings();
    await userEvent.click(
      await screen.findByRole("switch", { name: "Separate speakers by voice" }),
    );
    await waitFor(() => expect(invokeCalls("set_meeting_settings")).toHaveLength(1));
    const saved = invokeCalls("set_meeting_settings")[0] as {
      settings: Record<string, unknown>;
    };
    expect(saved.settings.voice_separation).toBe(true);
    expect(invokeCalls("download_onnx_model")).toHaveLength(0);
  });

  // --- item 17: calendar titles ---

  it("asks for calendar access when the calendar toggle goes on", async () => {
    mockWorld({
      bindings: readyBindings,
      extra: {
        get_permission_status: () => "Denied",
        request_permission: () => "Granted",
      },
    });
    render(<MeetingsPage />);

    await openSettings();
    const toggle = await screen.findByRole("switch", {
      name: "Name meetings from your calendar",
    });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    // The privacy promise sits next to the toggle, not in a settings page.
    expect(screen.getByText(/never sent to an AI provider/)).toBeTruthy();

    await userEvent.click(toggle);

    await waitFor(() => expect(invokeCalls("request_permission")).toHaveLength(1));
    expect(invokeCalls("request_permission")[0]).toEqual({ kind: "calendar" });
    const saved = invokeCalls("set_meeting_settings")[0] as {
      settings: Record<string, unknown>;
    };
    expect(saved.settings.calendar_titles).toBe(true);
  });

  // Reading the status must never prompt; the toggle is what prompts.
  it("does not ask for calendar access on mount", async () => {
    mockWorld({ bindings: readyBindings, extra: { get_permission_status: () => "Denied" } });
    render(<MeetingsPage />);
    await screen.findByRole("heading", { level: 1, name: "Meetings" });
    expect(invokeCalls("request_permission")).toHaveLength(0);
    expect(
      invokeCalls("get_permission_status").some((args) => args?.kind === "calendar"),
    ).toBe(true);
  });

  // Attribution only produces a verdict when both channels were recorded.
  // A segment it could not decide reads as unattributed, not as a guess.
  it("leaves an ambiguous live segment unlabelled", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);
    await screen.findByRole("heading", { level: 1, name: "Meetings" });

    await act(async () => {
      emitTauriEvent("meeting:state", { state: "recording" });
      emitTauriEvent("meeting:segment", {
        meeting_id: "meeting-1",
        sequence: 0,
        start_offset_ms: 0,
        end_offset_ms: 5000,
        text: "crosstalk",
        speaker_key: "mixed",
      });
    });

    await userEvent.click(screen.getByRole("tab", { name: "Transcript" }));
    const line = (await screen.findByText("crosstalk")).closest("li");
    expect(line?.textContent).toBe("[00:00]crosstalk");
  });

  // --- settings slide-over ---

  it("keeps the settings out of the page until ⚙ opens them", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    const gear = await screen.findByRole("button", { name: "Meeting settings" });
    expect(gear.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("dialog")).toBeNull();

    const dialog = await openSettings();
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(gear.getAttribute("aria-expanded")).toBe("true");
  });

  // aria-modal says the page behind is inert; Tab must agree, or a keyboard
  // user walks out of the dialog on the first Shift+Tab.
  it("keeps Tab inside the open panel", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    const dialog = await openSettings();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Close" }));

    await userEvent.tab({ shift: true });
    expect(dialog.contains(document.activeElement)).toBe(true);
    await userEvent.tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Close" }));
  });

  it("closes on Escape and returns focus to the ⚙ button", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    const gear = await screen.findByRole("button", { name: "Meeting settings" });
    await userEvent.click(gear);
    await screen.findByRole("dialog", { name: "Meeting settings" });
    // Focus moves into the panel on open, so Escape is heard there.
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Close" }));

    await userEvent.keyboard("{Escape}");

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(gear);
  });

  it("closes from its Close button and from ⚙, returning focus each time", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    const gear = await screen.findByRole("button", { name: "Meeting settings" });
    await userEvent.click(gear);
    await userEvent.click(await screen.findByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(gear);

    // A second press of ⚙ is a toggle, not a reopen.
    await userEvent.click(gear);
    await screen.findByRole("dialog", { name: "Meeting settings" });
    await userEvent.click(gear);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(gear);
  });

  it("closes on an outside click and keeps a setting typed but not yet saved", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    await openSettings();
    const seconds = screen.getByRole("spinbutton", {
      name: "Seconds per transcript segment",
    });
    // A direct change rather than typing: userEvent cannot select the text of a
    // number field, and typing onto the old value would clamp to 120.
    fireEvent.change(seconds, { target: { value: "45" } });

    await userEvent.click(screen.getByRole("heading", { level: 1, name: "Meetings" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    // Reopened, the value the user typed is still there: closing hid the panel
    // and kept the page's state, rather than remounting it from the saved value.
    await openSettings();
    expect(
      (screen.getByRole("spinbutton", { name: "Seconds per transcript segment" }) as HTMLInputElement)
        .value,
    ).toBe("45");
    // Held locally, not written: this field only persists on blur.
    expect(invokeCalls("set_meeting_settings")).toHaveLength(0);
  });
});

// --- the workspace: list beside the selected meeting ---

const originalMatchMedia = window.matchMedia;

/** Answers the narrow-window query with `narrow`, and every other query with no match. */
function setNarrow(narrow: boolean) {
  window.matchMedia = ((query: string) => ({
    matches: narrow && query === "(max-width: 720px)",
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

const older = {
  ...meetingRow,
  id: "old",
  title: "Older review",
  started_at: "2026-07-10T09:00:00Z",
  ended_at: "2026-07-10T09:30:00Z",
};
const newer = {
  ...meetingRow,
  id: "new",
  title: "Newest sync",
  started_at: "2026-07-17T10:00:00Z",
  ended_at: "2026-07-17T10:30:00Z",
};

/** The title field of the open meeting, which is an input whenever it can be renamed. */
const titleValue = () => (screen.getByLabelText("Meeting title") as HTMLInputElement).value;

describe("MeetingsPage workspace", () => {
  beforeEach(() => resetTauriMocks());
  afterEach(() => {
    window.matchMedia = originalMatchMedia;
  });

  it("lays the list beside the selected meeting, both visible at once", async () => {
    mockWorld({ bindings: readyBindings, extra: { list_meetings: () => [older, newer] } });
    const { container } = render(<MeetingsPage />);

    await waitFor(() => expect(titleValue()).toBe("Newest sync"));
    expect(screen.getByRole("listbox", { name: "Meetings" })).toBeTruthy();
    expect(container.querySelector(".kea-meetings-grid:not(.kea-meetings-grid--narrow)")).not.toBeNull();
  });

  // Newest is the default: the summary the user most often came for.
  it("opens on the newest meeting when nothing is recording", async () => {
    mockWorld({ bindings: readyBindings, extra: { list_meetings: () => [older, newer] } });
    render(<MeetingsPage />);

    await waitFor(() => expect(titleValue()).toBe("Newest sync"));
    expect(screen.getByRole("option", { name: /Newest sync/ }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("option", { name: /Older review/ }).getAttribute("aria-selected")).toBe("false");
  });

  // Recording beats newest: the meeting in progress is what the user is after.
  it("opens on the meeting being recorded when one is", async () => {
    mockWorld({
      bindings: readyBindings,
      extra: {
        list_meetings: () => [
          older,
          { ...newer, id: "live", title: "Live planning", status: "recording", ended_at: null },
        ],
        get_meeting_state: () => ({ state: "recording", active_meeting_id: "live" }),
      },
    });
    render(<MeetingsPage />);

    await waitFor(() => expect(titleValue()).toBe("Live planning"));
    const pinned = screen.getByRole("option", { name: /Live planning/ });
    expect(pinned.getAttribute("aria-selected")).toBe("true");
    expect(screen.getAllByRole("option")[0]).toBe(pinned);
  });

  it("shows the empty state with a Record control when there are no meetings", async () => {
    mockWorld({ bindings: readyBindings });
    render(<MeetingsPage />);

    expect(await screen.findByRole("heading", { name: "No meetings yet" })).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Record a meeting" }));
    await waitFor(() => expect(invokeCalls("start_meeting")).toHaveLength(1));
  });

  // The selection belongs to the app shell, so the page follows what it is given.
  it("keeps the selection it is given, and asks for the newest when given none", async () => {
    mockWorld({ bindings: readyBindings, extra: { list_meetings: () => [older, newer] } });
    const onSelect = vi.fn();
    const { rerender } = render(
      <MeetingsPage selectedMeetingId="old" onSelectMeeting={onSelect} />,
    );

    await waitFor(() => expect(titleValue()).toBe("Older review"));
    expect(onSelect).not.toHaveBeenCalled();

    rerender(<MeetingsPage selectedMeetingId={null} onSelectMeeting={onSelect} />);
    await waitFor(() => expect(onSelect).toHaveBeenCalledWith("new"));
  });

  // Leaving the page unmounts it; the selection has to survive in the shell.
  it("restores the selected meeting when the user comes back to Meetings", async () => {
    mockWorld({ bindings: readyBindings, extra: { list_meetings: () => [older, newer] } });
    function Shell() {
      const [selected, setSelected] = useState<string | null>(null);
      const [onPage, setOnPage] = useState(true);
      return (
        <>
          <button type="button" onClick={() => setOnPage((v) => !v)}>
            Switch page
          </button>
          {onPage && <MeetingsPage selectedMeetingId={selected} onSelectMeeting={setSelected} />}
        </>
      );
    }
    render(<Shell />);

    await waitFor(() => expect(titleValue()).toBe("Newest sync"));
    await userEvent.click(screen.getByRole("option", { name: /Older review/ }));
    await waitFor(() => expect(titleValue()).toBe("Older review"));

    await userEvent.click(screen.getByRole("button", { name: "Switch page" }));
    expect(screen.queryByRole("listbox")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Switch page" }));

    await waitFor(() => expect(titleValue()).toBe("Older review"));
  });

  // Below the two-pane width the list and the meeting take turns.
  it("shows one pane at a time in a narrow window, with Back to the list", async () => {
    setNarrow(true);
    mockWorld({ bindings: readyBindings, extra: { list_meetings: () => [older, newer] } });
    render(<MeetingsPage />);

    expect(await screen.findByRole("listbox", { name: "Meetings" })).toBeTruthy();
    expect(screen.queryByLabelText("Meeting title")).toBeNull();

    await userEvent.click(screen.getByRole("option", { name: /Older review/ }));
    expect(await screen.findByLabelText("Meeting title")).toBeTruthy();
    expect(screen.queryByRole("listbox", { name: "Meetings" })).toBeNull();

    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByRole("listbox", { name: "Meetings" })).toBeTruthy();
    expect(screen.getByRole("option", { name: /Older review/ }).getAttribute("aria-selected")).toBe("true");
    expect(screen.queryByLabelText("Meeting title")).toBeNull();
  });

  // --- live view through a stop ---

  const notesFor = (meetingId: string, summary: string) => ({
    meeting_id: meetingId,
    summary,
    decisions: "",
    action_items: "",
    follow_ups: "",
    open_questions: "",
    prompt_version: "meeting-notes-v3",
    engine_id: "openai",
    model: "gpt-4o-mini",
    edited: false,
  });

  const storedMeeting = (notes: unknown) => ({
    meeting: {
      ...meetingRow,
      id: "live",
      title: "Planning",
      started_at: "2026-10-23T09:00:00Z",
      ended_at: "2026-10-23T09:20:00Z",
    },
    segments: [],
    notes,
    speakers: [],
    action_items: [],
    can_undo: false,
  });

  it("follows a meeting from its live view through stop to the stored notes", async () => {
    let finishStop: (value: unknown) => void = () => {};
    let finalNotesReady = false;
    const storedRow = { ...meetingRow, id: "live", title: "Planning", started_at: "2026-10-23T09:00:00Z", ended_at: "2026-10-23T09:20:00Z" };
    mockWorld({
      bindings: readyBindings,
      extra: {
        // The list picks the meeting up once it is stored.
        list_meetings: () => (finalNotesReady ? [older, storedRow] : [older]),
        stop_meeting: () => new Promise((resolve) => (finishStop = resolve)),
        get_meeting: (args) => {
          const id = (args as { id: string }).id;
          if (id !== "live") return { ...storedMeeting(null), meeting: older, segments: [] };
          return storedMeeting(
            finalNotesReady ? notesFor("live", "Final summary.") : null,
          );
        },
      },
    });
    render(<MeetingsPage />);
    await waitFor(() => expect(titleValue()).toBe("Older review"));

    // Recording starts: the live meeting takes the view and its notes arrive.
    await act(async () => {
      emitTauriEvent("meeting:state", { state: "recording" });
      emitTauriEvent("meeting:notes", notesFor("live", "Early summary."));
    });
    expect(await screen.findByText("Early summary.")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Notes so far" })).toBeTruthy();

    await act(async () => {
      emitTauriEvent("meeting:segment", {
        meeting_id: "live",
        sequence: 0,
        start_offset_ms: 0,
        end_offset_ms: 4000,
        text: "kicking off",
        speaker_key: "local",
      });
    });
    await userEvent.click(screen.getByRole("tab", { name: "Transcript" }));
    expect(screen.getByText("kicking off")).toBeTruthy();

    // Stop: the backend reports that it is finishing the notes, then returns.
    await userEvent.click(screen.getByRole("button", { name: "Stop meeting" }));
    await waitFor(() => expect(invokeCalls("stop_meeting")).toHaveLength(1));
    await act(async () => {
      emitTauriEvent("meeting:state", { state: "processing" });
    });
    await userEvent.click(screen.getByRole("tab", { name: "Summary" }));
    expect(await screen.findByText("Generating notes…")).toBeTruthy();

    // The notes are written: the stop returns and the stored meeting is read back.
    finalNotesReady = true;
    await act(async () => {
      emitTauriEvent("meeting:state", { state: "idle" });
      finishStop(storedMeeting(notesFor("live", "Final summary.")));
    });

    expect(await screen.findByText("Final summary.")).toBeTruthy();
    expect(screen.queryByText("Generating notes…")).toBeNull();
    expect(screen.queryByText("Early summary.")).toBeNull();
  });

  // Found in review: only the Stop button re-read the finished meeting, so a
  // stop from the hotkey (no stop_meeting call from this page) left the pane
  // without its notes until the user reselected the meeting.
  it("shows the final notes after a stop that did not come from the button", async () => {
    let finalNotesReady = false;
    const storedRow = { ...meetingRow, id: "live", title: "Planning", started_at: "2026-10-23T09:00:00Z", ended_at: "2026-10-23T09:20:00Z" };
    mockWorld({
      bindings: readyBindings,
      extra: {
        list_meetings: () => (finalNotesReady ? [older, storedRow] : [older]),
        get_meeting: (args) => {
          const id = (args as { id: string }).id;
          if (id !== "live") return { ...storedMeeting(null), meeting: older, segments: [] };
          return storedMeeting(finalNotesReady ? notesFor("live", "Final summary.") : null);
        },
      },
    });
    render(<MeetingsPage />);
    await waitFor(() => expect(titleValue()).toBe("Older review"));

    await act(async () => {
      emitTauriEvent("meeting:state", { state: "recording" });
      emitTauriEvent("meeting:notes", notesFor("live", "Early summary."));
    });
    expect(await screen.findByText("Early summary.")).toBeTruthy();

    await act(async () => {
      emitTauriEvent("meeting:state", { state: "processing" });
    });
    finalNotesReady = true;
    await act(async () => {
      emitTauriEvent("meeting:state", { state: "idle" });
    });

    expect(await screen.findByText("Final summary.")).toBeTruthy();
    expect(invokeCalls("stop_meeting")).toHaveLength(0);
  });
});

describe("MeetingsPage notes changes", () => {
  beforeEach(() => resetTauriMocks());

  // Apply on the Ask AI tab returns the fresh detail, which the page must hand
  // to the pane so the Summary shows the new notes without a reload.
  it("shows an applied proposal on the Summary tab, passed up through the page", async () => {
    const generatedNotes = {
      meeting_id: "new",
      summary: "The launch slips a week.",
      decisions: "",
      action_items: "",
      follow_ups: "",
      open_questions: "",
      prompt_version: "meeting-notes-v3",
      engine_id: "openai",
      model: "gpt-4o-mini",
      edited: false,
    };
    let detail = {
      meeting: newer,
      segments: [],
      notes: generatedNotes,
      speakers: [],
      action_items: [],
      can_undo: false,
    };
    const proposalState = { state: "pending" as "pending" | "applied" };
    const chat = () => [
      {
        id: 2,
        role: "assistant",
        content: "Here is a shorter summary.",
        created_at: "2026-07-17T10:31:00Z",
        proposal: {
          proposal: { ...generatedNotes, summary: "Launch slips a week." },
          state: proposalState.state,
          changed: ["summary"],
        },
      },
    ];
    mockWorld({
      bindings: readyBindings,
      extra: {
        list_meetings: () => [newer],
        get_meeting: () => detail,
        get_meeting_chat: chat,
        apply_notes_proposal: () => {
          proposalState.state = "applied";
          detail = {
            ...detail,
            notes: { ...generatedNotes, summary: "Launch slips a week.", edited: true },
            can_undo: true,
          };
          return detail;
        },
      },
    });
    render(<MeetingsPage />);
    await waitFor(() => expect(titleValue()).toBe("Newest sync"));

    await userEvent.click(screen.getByRole("tab", { name: "Ask AI" }));
    await userEvent.click(await screen.findByRole("button", { name: "Apply" }));
    expect(await screen.findByText("Applied to the notes.")).toBeTruthy();
    expect(invokeCalls("apply_notes_proposal")).toEqual([{ meetingId: "new", messageId: 2 }]);

    await userEvent.click(screen.getByRole("tab", { name: "Summary" }));
    expect(await screen.findByText("Launch slips a week.")).toBeTruthy();
    expect(screen.queryByText("The launch slips a week.")).toBeNull();
    expect(screen.getByText("edited")).toBeTruthy();
  });
});
