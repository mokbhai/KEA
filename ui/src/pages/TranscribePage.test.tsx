import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  emitTauriEvent,
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import TranscribePage from "./TranscribePage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

/**
 * The webview drag-drop channel, mocked as its own module.
 *
 * This is the whole reason the page does not use a React `onDrop`: with
 * Tauri v2's native drag-drop on — which it is, by default — an HTML5 drop
 * handler never fires, so a test driving `fireEvent.drop` would pass against
 * a page that does nothing in the real app.
 */
let dropHandler: ((event: { payload: unknown }) => void) | null = null;
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (handler: (event: { payload: unknown }) => void) => {
      dropHandler = handler;
      return Promise.resolve(() => {
        dropHandler = null;
      });
    },
  }),
}));

const rows = [
  {
    id: "tr-1",
    source_path: "/Users/me/Recordings/standup.m4a",
    source_filename: "standup.m4a",
    duration_ms: 95_000,
    stt_engine_id: "whisper",
    model: "ggml-base.en",
    language: null,
    status: "completed",
    error: null,
    created_at: "2026-09-19T10:00:00Z",
  },
];

const detail = {
  transcript: rows[0],
  segments: [
    {
      id: 1,
      transcript_id: "tr-1",
      sequence: 0,
      start_ms: 0,
      end_ms: 1_500,
      text: "Morning everyone",
      speaker_key: "spk0",
    },
  ],
};

const handlers = {
  list_transcripts: () => rows,
  get_transcript: () => detail,
  transcribe_file: () => "tr-1",
  cancel_file_transcription: () => undefined,
  export_transcript: () => "/Users/me/Recordings/standup.srt",
  render_transcript_subtitles: () => "1\n00:00:00,000 --> 00:00:01,500\nMorning everyone\n\n",
  delete_transcript: () => undefined,
  pick_audio_file: () => "/Users/me/Recordings/picked.mp3",
};

async function drop(paths: string[]) {
  await act(async () => {
    dropHandler?.({ payload: { type: "drop", paths } });
  });
}

describe("TranscribePage", () => {
  beforeEach(() => {
    resetTauriMocks();
    dropHandler = null;
    onInvoke(handlers);
  });

  it("lists previously transcribed files", async () => {
    render(<TranscribePage />);
    expect(await screen.findByText("standup.m4a")).toBeTruthy();
  });

  /// The path a real drop takes, which is the Tauri event and not `onDrop`.
  it("starts a job from the webview drag-drop event", async () => {
    render(<TranscribePage />);
    await screen.findByText("standup.m4a");
    await drop(["/Users/me/Recordings/interview.mp4"]);
    await waitFor(() =>
      expect(invokeCalls("transcribe_file")).toEqual([
        { path: "/Users/me/Recordings/interview.mp4" },
      ]),
    );
  });

  it("refuses a dropped file that is not a recording", async () => {
    render(<TranscribePage />);
    await screen.findByText("standup.m4a");
    await drop(["/Users/me/contract.pdf"]);
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(invokeCalls("transcribe_file")).toEqual([]);
  });

  /// The reason the segment event exists: a 40-minute recording has to show
  /// text as it lands rather than a spinner.
  it("streams cues as they arrive", async () => {
    render(<TranscribePage />);
    await screen.findByText("standup.m4a");
    await act(async () => {
      emitTauriEvent("transcribe:file:segment", {
        job_id: "job-1",
        start_ms: 30_200,
        end_ms: 32_000,
        text: "second chunk speech",
      });
    });
    expect(await screen.findByText("second chunk speech")).toBeTruthy();
    // Rebased onto the source timeline, so the offset is 00:30 and not 00:00.
    expect(screen.getByText("[00:30]")).toBeTruthy();
  });

  it("shows a speaker chip when a cue carries one", async () => {
    render(<TranscribePage />);
    const open = await screen.findByText("standup.m4a");
    await userEvent.click(open);
    expect(await screen.findByText("spk0")).toBeTruthy();
  });

  /// A cancel completes the job; it does not fail it, and the partial
  /// transcript stays exportable.
  it("reports a cancel as a stop rather than an error", async () => {
    render(<TranscribePage />);
    await screen.findByText("standup.m4a");
    await act(async () => {
      emitTauriEvent("transcribe:file:complete", {
        job_id: "job-1",
        transcript_id: "tr-1",
        cancelled: true,
      });
    });
    expect(await screen.findByText(/Stopped/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("exports the selected transcript as SRT", async () => {
    render(<TranscribePage />);
    await userEvent.click(await screen.findByText("standup.m4a"));
    await userEvent.click(await screen.findByText("Export SRT"));
    await waitFor(() =>
      expect(invokeCalls("export_transcript")).toEqual([
        { id: "tr-1", format: "srt", destination: undefined },
      ]),
    );
  });

  it("starts a job from the file picker", async () => {
    render(<TranscribePage />);
    await screen.findByText("standup.m4a");
    await userEvent.click(screen.getByText("Choose file…"));
    await waitFor(() =>
      expect(invokeCalls("transcribe_file")).toEqual([
        { path: "/Users/me/Recordings/picked.mp3" },
      ]),
    );
  });

  it("surfaces a backend error", async () => {
    render(<TranscribePage />);
    await screen.findByText("standup.m4a");
    await act(async () => {
      emitTauriEvent("transcribe:file:error", {
        job_id: "job-1",
        message: "no audio track this build can decode",
      });
    });
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByText(/no audio track/)).toBeTruthy();
  });
});
