import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MeetingDetail as MeetingDetailData, MeetingNotes } from "../../api";
import { invokeCalls, onInvoke, resetTauriMocks } from "../../test-utils/tauri";

vi.mock("@tauri-apps/api/core", async () => (await import("../../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../../test-utils/tauri")).eventModule);

beforeEach(() => {
  resetTauriMocks();
});
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

  // With live notes off nothing arrives until stop; the placeholder must say so.
  it("does not promise live notes when they are switched off", () => {
    renderSummary({ detail: null, live: true, liveNotes: null, interimNotes: false });
    expect(screen.getByText(/Notes are written when the meeting ends/)).toBeTruthy();
    expect(screen.queryByText("Notes appear here as the meeting runs.")).toBeNull();
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

  // --- direct editing (task 5.4) ---

  /**
   * The page's wiring in miniature: the tab's detail lives in state, and
   * onNotesChanged replaces it, as MeetingsPage does with applyDetail.
   */
  function EditHarness({ start, live = false }: { start: MeetingDetailData; live?: boolean }) {
    const [detail, setDetail] = useState<MeetingDetailData>(start);
    return (
      <SummaryTab
        detail={live ? null : detail}
        live={live}
        liveNotes={null}
        generating={false}
        onNotesChanged={setDetail}
      />
    );
  }

  /** A stored notes row the backend would keep: edits add a version, undo pops it. */
  function storeWithUndo(initial: MeetingDetailData) {
    let stored = initial;
    const history: MeetingNotes[] = [];
    onInvoke({
      edit_notes_section: (args) => {
        const { section, text } = args as { section: keyof MeetingNotes; text: string };
        history.push({ ...stored.notes! });
        stored = {
          ...stored,
          notes: { ...stored.notes!, [section]: text, edited: true },
          can_undo: true,
        };
        return stored;
      },
      undo_notes_edit: () => {
        const previous = history.pop();
        stored = {
          ...stored,
          notes: previous ?? { ...stored.notes!, edited: false },
          can_undo: history.length > 0,
        };
        return stored;
      },
    });
  }

  it("edits a section, saves it through the backend, and undoes it back to the generated text", async () => {
    storeWithUndo(meetingDetail({ notes: meetingNotes }));
    render(<EditHarness start={meetingDetail({ notes: meetingNotes })} />);

    // Decisions is empty in the fixture but still editable.
    await userEvent.click(screen.getByRole("button", { name: "Edit decisions" }));
    const box = screen.getByLabelText("Decisions text");
    await userEvent.type(box, "Ship on Friday");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(invokeCalls("edit_notes_section")).toHaveLength(1));
    expect(invokeCalls("edit_notes_section")[0]).toEqual({
      meetingId: "m1",
      section: "decisions",
      text: "Ship on Friday",
    });
    expect(await screen.findByText("Ship on Friday")).toBeTruthy();
    expect(screen.getByText("edited")).toBeTruthy();
    expect(screen.queryByLabelText("Decisions text")).toBeNull();

    await userEvent.click(screen.getByRole("button", { name: "Undo" }));

    await waitFor(() => expect(screen.queryByText("Ship on Friday")).toBeNull());
    expect(screen.getByRole("button", { name: "Edit decisions" })).toBeTruthy();
    expect(screen.queryByText("edited")).toBeNull();
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
    expect(invokeCalls("undo_notes_edit")).toEqual([{ meetingId: "m1" }]);
  });

  it("cancel restores the section without saving anything", async () => {
    storeWithUndo(meetingDetail({ notes: meetingNotes }));
    render(<EditHarness start={meetingDetail({ notes: meetingNotes })} />);

    await userEvent.click(screen.getByRole("button", { name: "Edit summary" }));
    const box = screen.getByLabelText("Summary text") as HTMLTextAreaElement;
    await userEvent.type(box, " Really.");
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(invokeCalls("edit_notes_section")).toHaveLength(0);
    expect(screen.getByText("The launch slips a week.")).toBeTruthy();
    expect(screen.queryByLabelText("Summary text")).toBeNull();
  });

  it("does not save an edit that changed nothing, so the undo stack gets no empty step", async () => {
    storeWithUndo(meetingDetail({ notes: meetingNotes }));
    render(<EditHarness start={meetingDetail({ notes: meetingNotes })} />);

    await userEvent.click(screen.getByRole("button", { name: "Edit summary" }));
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(invokeCalls("edit_notes_section")).toHaveLength(0);
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });

  it("offers no Edit or Undo while the meeting records, and says why", () => {
    render(
      <EditHarness
        start={meetingDetail({ notes: meetingNotes, can_undo: true })}
        live
      />,
    );
    expect(screen.queryByRole("button", { name: /^Edit / })).toBeNull();
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
    expect(screen.getByText("Editing is available when the meeting ends.")).toBeTruthy();
  });

  it("offers no Edit while the meeting is recording even when it has notes", () => {
    renderSummary({
      detail: null,
      live: true,
      liveNotes: meetingNotes,
      onNotesChanged: vi.fn(),
    });
    expect(screen.queryByRole("button", { name: /^Edit / })).toBeNull();
    expect(screen.getByText("Editing is available when the meeting ends.")).toBeTruthy();
  });

  // Found in review: between stop and the final notes the backend still
  // refuses changes, so no Edit may be offered in that window either.
  it("offers no Edit while the final notes are being written", () => {
    renderSummary({
      detail: meetingDetail({ notes: meetingNotes }),
      generating: true,
      onNotesChanged: vi.fn(),
    });
    expect(screen.queryByRole("button", { name: /^Edit / })).toBeNull();
  });

  it("keeps one section open at a time so unsaved text is not thrown away", async () => {
    renderSummary({ detail: meetingDetail({ notes: meetingNotes }), onNotesChanged: vi.fn() });
    await userEvent.click(screen.getByRole("button", { name: "Edit summary" }));
    await userEvent.type(screen.getByLabelText("Summary text"), " more");
    expect((screen.getByRole("button", { name: "Edit decisions" }) as HTMLButtonElement).disabled).toBe(true);
  });
});
