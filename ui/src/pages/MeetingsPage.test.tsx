import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
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
  }),
  set_meeting_settings: () => undefined,
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

describe("MeetingsPage", () => {
  beforeEach(() => resetTauriMocks());

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

    const line = (await screen.findByText("crosstalk")).closest("li");
    expect(line?.textContent).toBe("[00:00]crosstalk");
  });
});
