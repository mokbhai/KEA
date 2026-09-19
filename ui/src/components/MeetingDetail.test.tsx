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
      title_source: "llm",
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
    action_items: [],
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

  // --- item 16b: action items as rows ---

  const notes = {
    meeting_id: "m1",
    summary: "The launch slips a week.",
    decisions: "",
    action_items: "Send the deck — Priya (by Friday)",
    follow_ups: "",
    open_questions: "",
    prompt_version: "meeting-notes-v3",
    engine_id: "openai",
    model: "gpt-4o-mini",
  };

  const items = [
    {
      id: 1,
      meeting_id: "m1",
      text: "Send the deck",
      owner: "Priya",
      due_hint: "by Friday",
      source_seq: 0,
      status: "open" as const,
    },
    {
      id: 2,
      meeting_id: "m1",
      text: "Book the room",
      owner: null,
      due_hint: null,
      source_seq: null,
      status: "done" as const,
    },
  ];

  it("renders action items as a checklist with owner and due hint", () => {
    render(
      <MeetingDetail
        detail={detail({ notes, action_items: items })}
        onDelete={() => {}}
      />,
    );
    const first = screen.getByRole("checkbox", { name: "Send the deck" });
    expect((first as HTMLInputElement).checked).toBe(false);
    expect((screen.getByRole("checkbox", { name: "Book the room" }) as HTMLInputElement).checked).toBe(
      true,
    );
    expect(screen.getByText("— Priya")).toBeTruthy();
    expect(screen.getByText("(by Friday)")).toBeTruthy();
  });

  it("ticks an item off through the callback", async () => {
    const onSetActionItemStatus = vi.fn();
    render(
      <MeetingDetail
        detail={detail({ notes, action_items: items })}
        onDelete={() => {}}
        onSetActionItemStatus={onSetActionItemStatus}
      />,
    );
    await userEvent.click(screen.getByRole("checkbox", { name: "Send the deck" }));
    expect(onSetActionItemStatus).toHaveBeenCalledWith(1, "done");
    await userEvent.click(screen.getByRole("checkbox", { name: "Book the room" }));
    expect(onSetActionItemStatus).toHaveBeenCalledWith(2, "open");
  });

  it("adds what the notes missed, and ignores an empty box", async () => {
    const onAddActionItem = vi.fn();
    render(
      <MeetingDetail
        detail={detail({ notes, action_items: items })}
        onDelete={() => {}}
        onAddActionItem={onAddActionItem}
      />,
    );
    const add = screen.getByRole("button", { name: "Add item" });
    expect(add.hasAttribute("disabled")).toBe(true);
    await userEvent.type(screen.getByLabelText("New action item"), "Draft the summary");
    await userEvent.click(add);
    expect(onAddActionItem).toHaveBeenCalledTimes(1);
    expect(onAddActionItem).toHaveBeenCalledWith("Draft the summary");
  });

  // A meeting recorded before the table existed still renders its action
  // items — from the prose column the rows are derived from.
  it("falls back to the prose column when a meeting predates the table", () => {
    render(<MeetingDetail detail={detail({ notes })} onDelete={() => {}} />);
    expect(screen.getByText("Send the deck — Priya (by Friday)")).toBeTruthy();
    expect(screen.queryByRole("checkbox")).toBeNull();
  });

  // --- item 16c: export ---

  it("shows export buttons only when the page wires them up", async () => {
    const onCopyMarkdown = vi.fn();
    const { rerender } = render(
      <MeetingDetail detail={detail()} onDelete={() => {}} />,
    );
    expect(screen.queryByRole("button", { name: "Copy Markdown" })).toBeNull();

    rerender(
      <MeetingDetail
        detail={detail()}
        onDelete={() => {}}
        onCopyMarkdown={onCopyMarkdown}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Copy Markdown" }));
    expect(onCopyMarkdown).toHaveBeenCalledTimes(1);
    // Save is a separate opt-in, not a package deal with copy.
    expect(screen.queryByRole("button", { name: "Save as Markdown" })).toBeNull();
    // And so is Notion, which the page withholds until it is set up.
    expect(screen.queryByRole("button", { name: "Send to Notion" })).toBeNull();
  });

  it("sends to Notion when the page offers that destination", async () => {
    const onExportNotion = vi.fn();
    render(
      <MeetingDetail
        detail={detail()}
        onDelete={() => {}}
        onExportNotion={onExportNotion}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Send to Notion" }));
    expect(onExportNotion).toHaveBeenCalledTimes(1);
  });

  // --- item 17: calendar titles ---

  it("chips a calendar title and leaves a generated one unmarked", () => {
    const { rerender } = render(
      <MeetingDetail
        detail={detail({ meeting: { ...detail().meeting, title_source: "calendar" } })}
        onDelete={() => {}}
      />,
    );
    expect(screen.getByText("from Calendar")).toBeTruthy();

    rerender(
      <MeetingDetail
        detail={detail({ meeting: { ...detail().meeting, title_source: "llm" } })}
        onDelete={() => {}}
      />,
    );
    expect(screen.queryByText("from Calendar")).toBeNull();
  });

  // A calendar title is sometimes right about the event and wrong about what
  // the recording contains; fixing it must not mean deleting anything.
  it("renames the meeting inline, once, on commit", async () => {
    const onRenameTitle = vi.fn();
    render(
      <MeetingDetail
        detail={detail()}
        onDelete={() => {}}
        onRenameTitle={onRenameTitle}
      />,
    );
    const input = screen.getByLabelText("Meeting title");
    await userEvent.clear(input);
    await userEvent.type(input, "Q3 Roadmap Review{Enter}");
    expect(onRenameTitle).toHaveBeenCalledTimes(1);
    expect(onRenameTitle).toHaveBeenCalledWith("Q3 Roadmap Review");
  });

  it("does not save an empty or unchanged title", async () => {
    const onRenameTitle = vi.fn();
    render(
      <MeetingDetail
        detail={detail()}
        onDelete={() => {}}
        onRenameTitle={onRenameTitle}
      />,
    );
    const input = screen.getByLabelText("Meeting title");
    await userEvent.clear(input);
    await userEvent.tab();
    await userEvent.click(input);
    await userEvent.tab();
    expect(onRenameTitle).not.toHaveBeenCalled();
  });
});
