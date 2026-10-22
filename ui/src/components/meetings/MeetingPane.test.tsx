import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Meeting } from "../../api";
import { invokeCalls, onInvoke, resetTauriMocks } from "../../test-utils/tauri";
import { meetingDetail, meetingNotes } from "../../test-utils/meetingFixtures";
import type { TranscriptSegment } from "../TranscriptPanel";
import MeetingPane, { type MeetingPaneHandle, type MeetingPaneProps } from "./MeetingPane";

// The Ask AI tab loads its chat over IPC, so the pane tests route commands too.
vi.mock("@tauri-apps/api/core", async () => (await import("../../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../../test-utils/tauri")).eventModule);

beforeEach(() => {
  resetTauriMocks();
  // Any test that opens the Ask AI tab gets an empty saved chat unless it says otherwise.
  onInvoke({ get_meeting_chat: () => [] });
});

const liveMeeting: Meeting = {
  id: "m-live",
  title: "Live planning",
  started_at: "2026-10-23T09:00:00Z",
  ended_at: null,
  status: "recording",
  capture_mode: "mic_only",
  stt_engine_id: null,
  llm_engine_id: null,
  error: null,
  title_source: "llm",
};

function renderPane(over: Partial<MeetingPaneProps> = {}) {
  const props: MeetingPaneProps = {
    meeting: meetingDetail().meeting,
    detail: meetingDetail({ notes: meetingNotes }),
    live: false,
    liveSegments: [],
    liveNotes: null,
    generating: false,
    loading: false,
    busy: false,
    onDelete: () => {},
    ...over,
  };
  return render(<MeetingPane {...props} />);
}

/** The Summary, Transcript and Ask AI tabs, in order. */
const tab = (name: string) => screen.getByRole("tab", { name });

describe("MeetingPane tabs", () => {
  // Summary is the default: the notes are what the user came for.
  it("opens on Summary with the three tabs in order", () => {
    renderPane();
    const tabs = screen.getAllByRole("tab");
    expect(tabs.map((t) => t.textContent)).toEqual(["Summary", "Transcript", "Ask AI"]);
    expect(tab("Summary").getAttribute("aria-selected")).toBe("true");
    expect(tab("Transcript").getAttribute("aria-selected")).toBe("false");
    expect(screen.getByRole("tablist")).toBeTruthy();
    expect(screen.getByRole("tabpanel").getAttribute("aria-labelledby")).toBe("meeting-tab-summary");
    expect(screen.getByText("The launch slips a week.")).toBeTruthy();
  });

  it("switches tabs on click", async () => {
    renderPane();
    await userEvent.click(tab("Transcript"));
    expect(tab("Transcript").getAttribute("aria-selected")).toBe("true");
    expect(screen.getByText("shall we start")).toBeTruthy();
    expect(screen.queryByText("The launch slips a week.")).toBeNull();
  });

  it("moves between tabs with the arrow keys, Home and End", async () => {
    renderPane();
    tab("Summary").focus();

    await userEvent.keyboard("{ArrowRight}");
    expect(tab("Transcript").getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(tab("Transcript"));

    await userEvent.keyboard("{ArrowRight}");
    expect(tab("Ask AI").getAttribute("aria-selected")).toBe("true");

    // Wraps around, as a tablist does.
    await userEvent.keyboard("{ArrowRight}");
    expect(tab("Summary").getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(tab("Summary"));

    await userEvent.keyboard("{ArrowLeft}");
    expect(tab("Ask AI").getAttribute("aria-selected")).toBe("true");

    await userEvent.keyboard("{Home}");
    expect(tab("Summary").getAttribute("aria-selected")).toBe("true");

    await userEvent.keyboard("{End}");
    expect(tab("Ask AI").getAttribute("aria-selected")).toBe("true");
  });

  it("keeps only the selected tab in the tab order", () => {
    renderPane();
    expect(tab("Summary").tabIndex).toBe(0);
    expect(tab("Transcript").tabIndex).toBe(-1);
    expect(tab("Ask AI").tabIndex).toBe(-1);
  });

  // The Ask AI tab loads the meeting's saved chat the first time it opens.
  it("opens the Ask AI tab, loading the saved chat for this meeting", async () => {
    onInvoke({ get_meeting_chat: () => [] });
    renderPane();
    await userEvent.click(tab("Ask AI"));
    expect(await screen.findByLabelText("Ask about this meeting")).toBeTruthy();
    expect(invokeCalls("get_meeting_chat")).toEqual([{ meetingId: "m1" }]);
  });

  it("keeps a half-typed question when the reader visits the transcript and comes back", async () => {
    onInvoke({ get_meeting_chat: () => [] });
    renderPane();
    await userEvent.click(tab("Ask AI"));
    const input = await screen.findByLabelText("Ask about this meeting");
    await userEvent.type(input, "what did Priya say");

    await userEvent.click(tab("Transcript"));
    await userEvent.click(tab("Ask AI"));

    expect((screen.getByLabelText("Ask about this meeting") as HTMLTextAreaElement).value).toBe(
      "what did Priya say",
    );
  });

  // A citation is a button in the chat. Clicking it must land on the transcript
  // line covering that time, and a time past the end must stay plain text.
  it("follows a citation to its transcript line through the pane", async () => {
    onInvoke({
      get_meeting_chat: () => [
        {
          id: 2,
          role: "assistant",
          content: "Priya covered it at [00:07]. It came back at [45:00].",
          proposal: null,
          created_at: "2026-09-19T10:20:00Z",
        },
      ],
    });
    renderPane();
    await userEvent.click(tab("Ask AI"));

    await userEvent.click(await screen.findByRole("button", { name: "[00:07]" }));

    expect(tab("Transcript").getAttribute("aria-selected")).toBe("true");
    await waitFor(() =>
      expect(screen.getByText("yes go ahead").closest("li")?.getAttribute("data-highlighted")).toBe(
        "true",
      ),
    );
    expect(screen.getByText("shall we start").closest("li")?.getAttribute("data-highlighted")).toBeNull();
    expect(screen.queryByRole("button", { name: "[45:00]" })).toBeNull();
    expect(screen.getByText("[45:00]").tagName).toBe("SPAN");
  });

  // --- per-tab scroll memory ---

  it("keeps each tab's scroll position when switching away and back", async () => {
    renderPane();
    const body = screen.getByRole("tabpanel");
    Object.defineProperty(body, "scrollTop", { configurable: true, writable: true, value: 120 });
    fireEvent.scroll(body);

    await userEvent.click(tab("Transcript"));
    expect(body.scrollTop).toBe(0);
    Object.defineProperty(body, "scrollTop", { configurable: true, writable: true, value: 40 });
    fireEvent.scroll(body);

    await userEvent.click(tab("Summary"));
    expect(body.scrollTop).toBe(120);

    await userEvent.click(tab("Transcript"));
    expect(body.scrollTop).toBe(40);
  });

  // --- title ---

  it("renames the meeting inline, once, on commit", async () => {
    const onRenameTitle = vi.fn();
    renderPane({ onRenameTitle });
    const input = screen.getByLabelText("Meeting title");
    await userEvent.clear(input);
    await userEvent.type(input, "Q3 Roadmap Review{Enter}");
    expect(onRenameTitle).toHaveBeenCalledTimes(1);
    expect(onRenameTitle).toHaveBeenCalledWith("Q3 Roadmap Review");
  });

  it("does not save an empty or unchanged title", async () => {
    const onRenameTitle = vi.fn();
    renderPane({ onRenameTitle });
    const input = screen.getByLabelText("Meeting title");
    await userEvent.clear(input);
    await userEvent.tab();
    await userEvent.click(input);
    await userEvent.tab();
    expect(onRenameTitle).not.toHaveBeenCalled();
  });

  it("chips a calendar title and leaves a generated one unmarked", () => {
    const calendar = renderPane({
      meeting: { ...meetingDetail().meeting, title_source: "calendar" },
    });
    expect(screen.getByText("from Calendar")).toBeTruthy();
    calendar.unmount();

    renderPane({ meeting: { ...meetingDetail().meeting, title_source: "llm" } });
    expect(screen.queryByText("from Calendar")).toBeNull();
  });

  // --- transcript through the pane ---

  it("labels each transcript line with its speaker and leaves a mixed one bare", async () => {
    renderPane();
    await userEvent.click(tab("Transcript"));
    expect(screen.getByText("You")).toBeTruthy();
    expect(screen.getByText("Others")).toBeTruthy();
    expect(screen.getByText("crosstalk").closest("li")?.textContent).toBe("[00:09]crosstalk");
  });

  it("jumps to a time from the handle, switching to the Transcript tab first", async () => {
    const ref = createRef<MeetingPaneHandle>();
    render(
      <MeetingPane
        ref={ref}
        meeting={meetingDetail().meeting}
        detail={meetingDetail({ notes: meetingNotes })}
        live={false}
        liveSegments={[]}
        liveNotes={null}
        generating={false}
        loading={false}
        busy={false}
        onDelete={() => {}}
      />,
    );
    expect(tab("Summary").getAttribute("aria-selected")).toBe("true");

    act(() => {
      ref.current?.jumpTo(7_000);
    });

    expect(tab("Transcript").getAttribute("aria-selected")).toBe("true");
    await waitFor(() =>
      expect(screen.getByText("yes go ahead").closest("li")?.getAttribute("data-highlighted")).toBe("true"),
    );
  });

  // --- live view ---

  it("renders the live notes and live transcript while the meeting records", async () => {
    const liveSegments: TranscriptSegment[] = [
      { meeting_id: "m-live", sequence: 0, start_offset_ms: 0, end_offset_ms: 4_000, text: "kicking off", speaker: "You" },
    ];
    renderPane({
      meeting: liveMeeting,
      detail: null,
      live: true,
      liveSegments,
      liveNotes: { ...meetingNotes, meeting_id: "m-live", summary: "Early summary." },
    });
    expect(screen.getByRole("heading", { name: "Notes so far" })).toBeTruthy();
    expect(screen.getByText("Early summary.")).toBeTruthy();

    await userEvent.click(tab("Transcript"));
    expect(screen.getByText("kicking off")).toBeTruthy();
  });

  it("says it is listening while a meeting records and has no lines yet", async () => {
    renderPane({ meeting: liveMeeting, detail: null, live: true, liveSegments: [] });
    await userEvent.click(tab("Transcript"));
    expect(screen.getByText("Listening — segments appear every few seconds…")).toBeTruthy();
  });

  it("does not offer delete on a meeting that is still recording", () => {
    renderPane({ meeting: liveMeeting, detail: null, live: true, onDelete: undefined });
    expect(screen.queryByRole("button", { name: "Delete meeting" })).toBeNull();
  });

  it("shows loading in place of the detail until it arrives", () => {
    renderPane({ detail: null, loading: true });
    expect(screen.getByText("Loading meeting…")).toBeTruthy();
  });

  it("ignores a detail that belongs to another meeting", () => {
    renderPane({
      meeting: { ...meetingDetail().meeting, id: "other", title: "Other meeting" },
      detail: meetingDetail({ notes: meetingNotes }),
    });
    expect(screen.queryByText("The launch slips a week.")).toBeNull();
  });

  // --- narrow layout ---

  it("shows a Back control only when the page gives one, and calls it", async () => {
    const onBack = vi.fn();
    const { rerender } = renderPane();
    expect(screen.queryByRole("button", { name: "Back" })).toBeNull();

    rerender(
      <MeetingPane
        meeting={meetingDetail().meeting}
        detail={meetingDetail({ notes: meetingNotes })}
        live={false}
        liveSegments={[]}
        liveNotes={null}
        generating={false}
        loading={false}
        busy={false}
        onBack={onBack}
        onDelete={() => {}}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(onBack).toHaveBeenCalledTimes(1);
  });
});
