import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
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
  get_meeting_settings: () => ({ segment_duration_secs: 30, prefer_system_audio: true }),
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
      status: "done",
      capture_mode: "mic_only",
      stt_engine_id: "whisper",
      llm_engine_id: "openai",
      error: null,
    },
    segments: [],
    notes: null,
  }),
  get_meeting: () => ({
    meeting: {
      id: "meeting-1",
      title: "Test capture",
      started_at: "2026-07-17T10:00:00Z",
      ended_at: "2026-07-17T10:00:10Z",
      status: "done",
      capture_mode: "mic_only",
      stt_engine_id: "whisper",
      llm_engine_id: "openai",
      error: null,
    },
    segments: [],
    notes: null,
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

      await act(async () => {
        vi.advanceTimersByTime(10_000);
      });

      await waitFor(() => expect(invokeCalls("stop_meeting")).toHaveLength(1));
    } finally {
      vi.useRealTimers();
    }
  });
});
