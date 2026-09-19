import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { MeetingDetail as MeetingDetailData } from "../api";
import MeetingDetail from "./MeetingDetail";

function detail(over: Record<string, unknown> = {}): MeetingDetailData {
  return {
    meeting: {
      id: "m1",
      title: "Weekly Sync",
      started_at: "2026-09-19T10:00:00Z",
      ended_at: "2026-09-19T10:30:00Z",
      status: "completed",
      capture_mode: "mic_and_system",
      stt_engine_id: "whisper",
      llm_engine_id: "openai",
      error: null,
    },
    segments: [
      {
        id: 1,
        meeting_id: "m1",
        sequence: 0,
        start_offset_ms: 0,
        end_offset_ms: 5_000,
        text: "shall we start",
        speaker_key: "local",
      },
      {
        id: 2,
        meeting_id: "m1",
        sequence: 1,
        start_offset_ms: 5_000,
        end_offset_ms: 9_000,
        text: "yes go ahead",
        speaker_key: "remote",
      },
      {
        id: 3,
        meeting_id: "m1",
        sequence: 2,
        start_offset_ms: 9_000,
        end_offset_ms: 12_000,
        text: "crosstalk",
        speaker_key: "mixed",
      },
    ],
    notes: null,
    speakers: [
      { meeting_id: "m1", speaker_key: "local", display_name: "You", source: "channel" },
      {
        meeting_id: "m1",
        speaker_key: "remote",
        display_name: "Others",
        source: "channel",
      },
    ],
    ...over,
  } as unknown as MeetingDetailData;
}

describe("MeetingDetail", () => {
  it("labels each transcript line with its speaker", () => {
    render(<MeetingDetail detail={detail()} onDelete={() => {}} />);
    expect(screen.getByText("shall we start")).toBeTruthy();
    expect(screen.getAllByText("You").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Others").length).toBeGreaterThan(0);
  });

  // An ambiguous segment reads as unattributed rather than being pushed onto
  // whichever side was marginally louder.
  it("leaves a mixed segment unlabelled", () => {
    render(<MeetingDetail detail={detail()} onDelete={() => {}} />);
    const line = screen.getByText("crosstalk").closest("li");
    expect(line?.textContent).toBe("[00:09]crosstalk");
  });

  it("names the physical side beside each speaker so a rename stays readable", () => {
    const onRenameSpeaker = vi.fn();
    render(
      <MeetingDetail
        detail={detail()}
        onDelete={() => {}}
        onRenameSpeaker={onRenameSpeaker}
      />,
    );
    expect(screen.getByLabelText("Name for Your mic")).toBeTruthy();
    expect(screen.getByLabelText("Name for System audio")).toBeTruthy();
  });

  it("saves a renamed speaker once, on commit", async () => {
    const onRenameSpeaker = vi.fn();
    render(
      <MeetingDetail
        detail={detail()}
        onDelete={() => {}}
        onRenameSpeaker={onRenameSpeaker}
      />,
    );
    const input = screen.getByLabelText("Name for System audio");
    await userEvent.clear(input);
    await userEvent.type(input, "Priya{Enter}");
    expect(onRenameSpeaker).toHaveBeenCalledTimes(1);
    expect(onRenameSpeaker).toHaveBeenCalledWith("remote", "Priya");
  });

  // An empty box is a slip mid-edit, and a name unchanged is not a write.
  it("does not save an empty or unchanged name", async () => {
    const onRenameSpeaker = vi.fn();
    render(
      <MeetingDetail
        detail={detail()}
        onDelete={() => {}}
        onRenameSpeaker={onRenameSpeaker}
      />,
    );
    const input = screen.getByLabelText("Name for System audio");
    await userEvent.clear(input);
    await userEvent.tab();
    await userEvent.click(input);
    await userEvent.tab();
    expect(onRenameSpeaker).not.toHaveBeenCalled();
  });

  // A meeting that recorded one source has one side; there is no second
  // speaker to invent, and nothing to rename it to.
  it("shows a single speaker for a mic-only meeting", () => {
    render(
      <MeetingDetail
        detail={detail({
          meeting: { ...detail().meeting, capture_mode: "mic_only" },
          speakers: [
            {
              meeting_id: "m1",
              speaker_key: "local",
              display_name: "You",
              source: "channel",
            },
          ],
        })}
        onDelete={() => {}}
        onRenameSpeaker={() => {}}
      />,
    );
    expect(screen.getByLabelText("Name for Your mic")).toBeTruthy();
    expect(screen.queryByLabelText("Name for System audio")).toBeNull();
  });

  // Meetings recorded before attribution existed have no speaker rows at all.
  it("omits the legend entirely when a meeting has no speakers", () => {
    render(
      <MeetingDetail detail={detail({ speakers: [] })} onDelete={() => {}} />,
    );
    expect(screen.queryByText("Speakers")).toBeNull();
  });
});
