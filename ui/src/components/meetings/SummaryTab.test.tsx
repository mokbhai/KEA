import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { MeetingNotes } from "../../api";
import { meetingActionItems, meetingDetail, meetingNotes } from "../../test-utils/meetingFixtures";
import SummaryTab, { type SummaryTabProps } from "./SummaryTab";

/** Renders the finished-meeting Summary with defaults, overridable per test. */
function renderSummary(over: Partial<SummaryTabProps> = {}) {
  const props: SummaryTabProps = {
    detail: meetingDetail(),
    live: false,
    liveNotes: null,
    generating: false,
    ...over,
  };
  return render(<SummaryTab {...props} />);
}

describe("SummaryTab", () => {
  // A name the user cannot see the side of is a name they cannot trust.
  it("names the physical side beside each speaker so a rename stays readable", () => {
    renderSummary({ onRenameSpeaker: vi.fn() });
    expect(screen.getByLabelText("Name for Your mic")).toBeTruthy();
    expect(screen.getByLabelText("Name for System audio")).toBeTruthy();
  });

  it("saves a renamed speaker once, on commit", async () => {
    const onRenameSpeaker = vi.fn();
    renderSummary({ onRenameSpeaker });
    const input = screen.getByLabelText("Name for System audio");
    await userEvent.clear(input);
    await userEvent.type(input, "Priya{Enter}");
    expect(onRenameSpeaker).toHaveBeenCalledTimes(1);
    expect(onRenameSpeaker).toHaveBeenCalledWith("remote", "Priya");
  });

  // An empty box is a slip mid-edit, and a name unchanged is not a write.
  it("does not save an empty or unchanged name", async () => {
    const onRenameSpeaker = vi.fn();
    renderSummary({ onRenameSpeaker });
    const input = screen.getByLabelText("Name for System audio");
    await userEvent.clear(input);
    await userEvent.tab();
    await userEvent.click(input);
    await userEvent.tab();
    expect(onRenameSpeaker).not.toHaveBeenCalled();
  });

  // A name the transcript proposed is marked as such, and confirming it is the
  // same write as renaming it to what it already says.
  it("marks an inferred name and confirms it as the same name", async () => {
    const onRenameSpeaker = vi.fn();
    renderSummary({
      detail: meetingDetail({
        speakers: [
          { meeting_id: "m1", speaker_key: "local", display_name: "You", source: "channel" },
          { meeting_id: "m1", speaker_key: "remote", display_name: "Priya", source: "inferred" },
        ],
      }),
      onRenameSpeaker,
    });
    expect(screen.getByText("inferred")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(onRenameSpeaker).toHaveBeenCalledTimes(1);
    expect(onRenameSpeaker).toHaveBeenCalledWith("remote", "Priya");
  });

  // Only a proposal needs confirming: a channel default or a name someone typed
  // gets neither the badge nor the button.
  it("offers no badge or confirm for a channel or user name", () => {
    renderSummary({
      detail: meetingDetail({
        speakers: [
          { meeting_id: "m1", speaker_key: "local", display_name: "You", source: "channel" },
          { meeting_id: "m1", speaker_key: "remote", display_name: "Dan", source: "user" },
        ],
      }),
      onRenameSpeaker: () => {},
    });
    expect(screen.queryByText("inferred")).toBeNull();
    expect(screen.queryByRole("button", { name: "Confirm" })).toBeNull();
  });

  // A meeting that recorded one source has one side; there is no second
  // speaker to invent, and nothing to rename it to.
  it("shows a single speaker for a mic-only meeting", () => {
    renderSummary({
      detail: meetingDetail({
        meeting: { ...meetingDetail().meeting, capture_mode: "mic_only" },
        speakers: [
          { meeting_id: "m1", speaker_key: "local", display_name: "You", source: "channel" },
        ],
      }),
      onRenameSpeaker: () => {},
    });
    expect(screen.getByLabelText("Name for Your mic")).toBeTruthy();
    expect(screen.queryByLabelText("Name for System audio")).toBeNull();
  });

  // Meetings recorded before attribution existed have no speaker rows at all.
  it("omits the legend entirely when a meeting has no speakers", () => {
    renderSummary({ detail: meetingDetail({ speakers: [] }) });
    expect(screen.queryByText("Speakers")).toBeNull();
  });

  // --- action items as rows ---

  const notesWithItems = { ...meetingNotes, meeting_id: "m1" };

  it("renders action items as a checklist with owner and due hint", () => {
    renderSummary({
      detail: meetingDetail({ notes: notesWithItems, action_items: meetingActionItems }),
    });
    const first = screen.getByRole("checkbox", { name: "Send the deck" });
    expect((first as HTMLInputElement).checked).toBe(false);
    expect(
      (screen.getByRole("checkbox", { name: "Book the room" }) as HTMLInputElement).checked,
    ).toBe(true);
    expect(screen.getByText("— Priya")).toBeTruthy();
    expect(screen.getByText("(by Friday)")).toBeTruthy();
  });

  it("ticks an item off through the callback", async () => {
    const onSetActionItemStatus = vi.fn();
    renderSummary({
      detail: meetingDetail({ notes: notesWithItems, action_items: meetingActionItems }),
      onSetActionItemStatus,
    });
    await userEvent.click(screen.getByRole("checkbox", { name: "Send the deck" }));
    expect(onSetActionItemStatus).toHaveBeenCalledWith(1, "done");
    await userEvent.click(screen.getByRole("checkbox", { name: "Book the room" }));
    expect(onSetActionItemStatus).toHaveBeenCalledWith(2, "open");
  });

  it("adds what the notes missed, and ignores an empty box", async () => {
    const onAddActionItem = vi.fn();
    renderSummary({
      detail: meetingDetail({ notes: notesWithItems, action_items: meetingActionItems }),
      onAddActionItem,
    });
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
    renderSummary({ detail: meetingDetail({ notes: notesWithItems }) });
    expect(screen.getByText("Send the deck — Priya (by Friday)")).toBeTruthy();
    expect(screen.queryByRole("checkbox")).toBeNull();
  });

  it("says so when a meeting has no action items at all", () => {
    renderSummary({
      detail: meetingDetail({
        notes: { ...meetingNotes, action_items: "" },
        action_items: [],
      }),
      onAddActionItem: () => {},
    });
    expect(screen.getByText("Nobody agreed to do anything.")).toBeTruthy();
  });

  // --- export and delete ---

  it("shows export buttons only when the page wires them up", async () => {
    const onCopyMarkdown = vi.fn();
    const { rerender } = render(<SummaryTab detail={meetingDetail()} live={false} liveNotes={null} generating={false} onDelete={() => {}} />);
    expect(screen.queryByRole("button", { name: "Copy Markdown" })).toBeNull();

    rerender(
      <SummaryTab
        detail={meetingDetail()}
        live={false}
        liveNotes={null}
        generating={false}
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
    renderSummary({ onExportNotion, onDelete: () => {} });
    await userEvent.click(screen.getByRole("button", { name: "Send to Notion" }));
    expect(onExportNotion).toHaveBeenCalledTimes(1);
  });

  it("saves as Markdown when the page wires the save action", async () => {
    const onSaveMarkdown = vi.fn();
    renderSummary({ onSaveMarkdown, onDelete: () => {} });
    await userEvent.click(screen.getByRole("button", { name: "Save as Markdown" }));
    expect(onSaveMarkdown).toHaveBeenCalledTimes(1);
  });

  it("deletes the meeting through the callback with its id", async () => {
    const onDelete = vi.fn();
    renderSummary({ onDelete });
    await userEvent.click(screen.getByRole("button", { name: "Delete meeting" }));
    expect(onDelete).toHaveBeenCalledWith("m1");
  });

  // --- edited notes and generation ---

  it("marks notes the user has changed with an edited badge", () => {
    const edited: MeetingNotes = { ...meetingNotes, edited: true };
    const { rerender } = renderSummary({ detail: meetingDetail({ notes: edited }) });
    expect(screen.getByText("edited")).toBeTruthy();

    rerender(
      <SummaryTab
        detail={meetingDetail({ notes: meetingNotes })}
        live={false}
        liveNotes={null}
        generating={false}
      />,
    );
    expect(screen.queryByText("edited")).toBeNull();
  });

  it("shows generating while the final notes are written, and not once they are", () => {
    const { rerender } = renderSummary({ detail: meetingDetail(), generating: true });
    expect(screen.getByRole("status").textContent).toBe("Generating notes…");

    rerender(
      <SummaryTab
        detail={meetingDetail({ notes: meetingNotes })}
        live={false}
        liveNotes={null}
        generating={false}
      />,
    );
    expect(screen.queryByText("Generating notes…")).toBeNull();
    expect(screen.getByText("The launch slips a week.")).toBeTruthy();
  });

  // --- live ---

  // Said plainly, because a stale summary read as a final one is how this
  // feature misleads.
  it("shows the live notes, labelled as unfinished, while the meeting records", () => {
    renderSummary({
      detail: null,
      live: true,
      liveNotes: { ...meetingNotes, summary: "Interim summary." },
    });
    expect(screen.getByRole("heading", { name: "Notes so far" })).toBeTruthy();
    expect(screen.getByText("Interim summary.")).toBeTruthy();
    expect(screen.getByText(/The full notes are written when you stop/)).toBeTruthy();
  });

  // A recording meeting has no stored record to export or delete yet.
  it("offers no export or delete while the meeting is still recording", () => {
    renderSummary({
      detail: null,
      live: true,
      liveNotes: null,
      onCopyMarkdown: () => {},
      onDelete: () => {},
    });
    expect(screen.queryByRole("button", { name: "Copy Markdown" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Delete meeting" })).toBeNull();
    expect(screen.getByText("Notes appear here as the meeting runs.")).toBeTruthy();
  });
});
