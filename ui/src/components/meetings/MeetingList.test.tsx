import { act, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Meeting } from "../../api";
import { invokeCalls, onInvoke, resetTauriMocks } from "../../test-utils/tauri";
import MeetingList from "./MeetingList";

vi.mock("@tauri-apps/api/core", async () => (await import("../../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../../test-utils/tauri")).eventModule);

/** A finished meeting that started at `started` (local time). */
function row(id: string, title: string, started: Date, over: Partial<Meeting> = {}): Meeting {
  return {
    id,
    title,
    started_at: started.toISOString(),
    ended_at: new Date(started.getTime() + 30 * 60 * 1000).toISOString(),
    status: "completed",
    capture_mode: "mic_only",
    stt_engine_id: "whisper",
    llm_engine_id: "openai",
    error: null,
    title_source: "llm",
    ...over,
  };
}

/** The group headings on screen, in order. */
function groupNames(): (string | null)[] {
  return screen.getAllByRole("group").map((g) => g.getAttribute("aria-label"));
}

function groupOf(label: string): HTMLElement {
  return screen.getByRole("group", { name: label });
}

const noop = () => {};

describe("MeetingList grouping", () => {
  beforeEach(() => resetTauriMocks());
  afterEach(() => vi.useRealTimers());

  // Local time, not UTC: a meeting at 23:50 last night is yesterday's, and
  // one at 00:10 is today's, whatever the UTC date says.
  it("groups by local calendar day, newest first, across midnight", () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(new Date(2026, 9, 23, 0, 30)); // Fri 23 Oct, half past midnight

    const meetings = [
      row("c", "Monday planning", new Date(2026, 9, 19, 9, 0)), // this week (Mon)
      row("b", "Just after midnight", new Date(2026, 9, 23, 0, 10)),
      row("a", "Late last night", new Date(2026, 9, 22, 23, 50)),
      row("e", "Earlier sync", new Date(2026, 8, 30, 9, 0)),
      row("d", "Tuesday sync", new Date(2026, 9, 20, 12, 0)),
    ];
    render(<MeetingList meetings={meetings} recording={null} selectedId={null} onSelect={noop} />);

    expect(groupNames()).toEqual(["Today", "Yesterday", "This week", "Earlier"]);
    expect(within(groupOf("Today")).getByText("Just after midnight")).toBeTruthy();
    expect(within(groupOf("Yesterday")).getByText("Late last night")).toBeTruthy();
    expect(within(groupOf("This week")).getByText("Tuesday sync")).toBeTruthy();
    expect(within(groupOf("This week")).getByText("Monday planning")).toBeTruthy();
    expect(within(groupOf("Earlier")).getByText("Earlier sync")).toBeTruthy();

    // Newest first within a group.
    const today = within(groupOf("This week")).getAllByRole("option").map((o) => o.textContent);
    expect(today[0]).toContain("Tuesday sync");
  });

  it("moves a meeting from Today to Yesterday when the clock passes midnight", () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(new Date(2026, 9, 22, 23, 59));
    const meetings = [row("x", "Crossing midnight", new Date(2026, 9, 22, 23, 58))];

    const { rerender } = render(
      <MeetingList meetings={meetings} recording={null} selectedId={null} onSelect={noop} />,
    );
    expect(within(groupOf("Today")).getByText("Crossing midnight")).toBeTruthy();

    // The list regroups on its next render, which is what a re-read would cause.
    act(() => vi.setSystemTime(new Date(2026, 9, 23, 0, 1)));
    rerender(<MeetingList meetings={meetings} recording={null} selectedId={null} onSelect={noop} />);
    expect(groupNames()).toEqual(["Yesterday"]);
    expect(within(groupOf("Yesterday")).getByText("Crossing midnight")).toBeTruthy();
  });

  it("says there are no meetings yet when the list is empty", () => {
    render(<MeetingList meetings={[]} recording={null} selectedId={null} onSelect={noop} />);
    expect(screen.getByText("No meetings yet.")).toBeTruthy();
    expect(screen.queryByRole("option")).toBeNull();
  });

  it("marks a meeting whose notes failed", () => {
    const failed = row("f", "Broken notes", new Date(), { status: "error", error: "boom" });
    render(<MeetingList meetings={[failed]} recording={null} selectedId={null} onSelect={noop} />);
    expect(screen.getByText("Failed")).toBeTruthy();
  });

  it("shows each row's start time and duration", () => {
    const started = new Date(2026, 9, 23, 9, 5);
    const meeting = row("d", "Sync", started, {
      ended_at: new Date(started.getTime() + 62 * 60 * 1000).toISOString(),
    });
    render(<MeetingList meetings={[meeting]} recording={null} selectedId={null} onSelect={noop} />);
    const option = screen.getByRole("option", { name: /Sync/ });
    expect(option.textContent).toContain(started.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }));
    expect(option.textContent).toContain("1 h 02 min");
  });
});

describe("MeetingList recording row", () => {
  beforeEach(() => resetTauriMocks());
  afterEach(() => vi.useRealTimers());

  // The meeting being recorded sits above the groups, with its elapsed time.
  it("pins the recording meeting above the groups, with a recording mark and elapsed time", () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(new Date(2026, 9, 23, 0, 30));
    const recording = row("live", "Standup", new Date(2026, 9, 23, 0, 29, 18), {
      status: "recording",
      ended_at: null,
    });
    const earlier = row("old", "Yesterday's review", new Date(2026, 9, 22, 16, 0));

    render(
      <MeetingList meetings={[earlier]} recording={recording} selectedId="live" onSelect={noop} />,
    );

    const options = screen.getAllByRole("option");
    expect(options[0].id).toBe("meeting-option-live");
    expect(options[0].getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("img", { name: "Recording" })).toBeTruthy();
    expect(options[0].textContent).toContain("00:42");
    expect(groupNames()).toEqual(["Yesterday"]);
  });

  it("does not repeat the recording meeting inside a group", () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(new Date(2026, 9, 23, 0, 30));
    const recording = row("live", "Standup", new Date(2026, 9, 23, 0, 29), {
      status: "recording",
      ended_at: null,
    });
    // The list is refreshed while recording, so the recording row can come
    // back in it; the page passes it in once, not twice.
    render(<MeetingList meetings={[]} recording={recording} selectedId={null} onSelect={noop} />);
    expect(screen.getAllByRole("option")).toHaveLength(1);
  });
});

describe("MeetingList search", () => {
  beforeEach(() => resetTauriMocks());
  afterEach(() => vi.useRealTimers());

  const now = new Date();
  const recent = [
    row("a", "Roadmap", new Date(now.getTime() - 60 * 60 * 1000)),
    row("b", "Hiring loop", new Date(now.getTime() - 2 * 60 * 60 * 1000)),
  ];

  // Typing fires a request per keystroke otherwise; the list waits for a pause.
  it("waits 200 ms after the last keystroke before searching", async () => {
    vi.useFakeTimers();
    onInvoke({
      search_meetings: () => [row("h", "Confluence migration", new Date())],
    });
    render(<MeetingList meetings={recent} recording={null} selectedId={null} onSelect={noop} />);

    const search = screen.getByRole("searchbox", { name: "Search meetings" });
    fireEvent.change(search, { target: { value: "conf" } });
    act(() => vi.advanceTimersByTime(199));
    expect(invokeCalls("search_meetings")).toHaveLength(0);

    // A further keystroke restarts the pause.
    fireEvent.change(search, { target: { value: "confl" } });
    act(() => vi.advanceTimersByTime(199));
    expect(invokeCalls("search_meetings")).toHaveLength(0);

    act(() => vi.advanceTimersByTime(1));
    expect(invokeCalls("search_meetings")).toEqual([{ query: "confl", limit: 200 }]);

    await act(async () => {});
    expect(screen.getByText("Confluence migration")).toBeTruthy();
    expect(screen.queryByText("Roadmap")).toBeNull();
  });

  it("shows the normal list again, at once, when the query is cleared", async () => {
    vi.useFakeTimers();
    onInvoke({ search_meetings: () => [] });
    render(<MeetingList meetings={recent} recording={null} selectedId={null} onSelect={noop} />);

    const search = screen.getByRole("searchbox", { name: "Search meetings" });
    fireEvent.change(search, { target: { value: "zzz" } });
    act(() => vi.advanceTimersByTime(200));
    await act(async () => {});
    expect(screen.queryByText("Roadmap")).toBeNull();

    // An empty box is the list, with no request and no pause.
    fireEvent.change(search, { target: { value: "" } });
    expect(screen.getByText("Roadmap")).toBeTruthy();
    act(() => vi.advanceTimersByTime(500));
    expect(invokeCalls("search_meetings")).toHaveLength(1);
  });

  it("says when nothing matches, and clears the search from there", async () => {
    vi.useFakeTimers();
    onInvoke({ search_meetings: () => [] });
    render(<MeetingList meetings={recent} recording={null} selectedId={null} onSelect={noop} />);

    const search = screen.getByRole("searchbox", { name: "Search meetings" }) as HTMLInputElement;
    fireEvent.change(search, { target: { value: "unicorn" } });
    act(() => vi.advanceTimersByTime(200));
    await act(async () => {});

    expect(screen.getByText("No meeting matches “unicorn”.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));
    expect(search.value).toBe("");
    expect(screen.getByText("Roadmap")).toBeTruthy();
    expect(screen.queryByText(/No meeting matches/)).toBeNull();
  });

  // Found in review: a search hid the meeting being recorded unless it
  // matched the query.
  it("keeps the recording meeting pinned while searching", async () => {
    vi.useFakeTimers();
    onInvoke({ search_meetings: () => [] });
    const live = row("live", "Standup", new Date(), { status: "recording", ended_at: null });
    render(<MeetingList meetings={recent} recording={live} selectedId={null} onSelect={noop} />);

    fireEvent.change(screen.getByRole("searchbox", { name: "Search meetings" }), {
      target: { value: "unicorn" },
    });
    act(() => vi.advanceTimersByTime(200));
    await act(async () => {});

    expect(screen.getByText("Standup")).toBeTruthy();
    expect(screen.getByText("No meeting matches “unicorn”.")).toBeTruthy();
  });

  it("reports a failed search rather than showing a stale list", async () => {
    vi.useFakeTimers();
    onInvoke({ search_meetings: () => { throw new Error("database is locked"); } });
    render(<MeetingList meetings={recent} recording={null} selectedId={null} onSelect={noop} />);

    fireEvent.change(screen.getByRole("searchbox", { name: "Search meetings" }), {
      target: { value: "road" },
    });
    act(() => vi.advanceTimersByTime(200));
    await act(async () => {});
    expect(screen.getByText("database is locked")).toBeTruthy();
  });
});

describe("MeetingList keyboard and selection", () => {
  beforeEach(() => resetTauriMocks());

  it("moves with the arrow keys, selects with Enter, and marks the selected row", async () => {
    const now = new Date();
    const meetings = [
      row("a", "First", new Date(now.getTime() - 60 * 60 * 1000)),
      row("b", "Second", new Date(now.getTime() - 2 * 60 * 60 * 1000)),
      row("c", "Third", new Date(now.getTime() - 3 * 60 * 60 * 1000)),
    ];
    const onSelect = vi.fn();
    render(<MeetingList meetings={meetings} recording={null} selectedId="b" onSelect={onSelect} />);

    const options = screen.getAllByRole("option");
    expect(options[1].getAttribute("aria-selected")).toBe("true");
    expect(options[0].getAttribute("aria-selected")).toBe("false");
    // Only the active row is in the tab order.
    expect(options[1].tabIndex).toBe(0);
    expect(options[0].tabIndex).toBe(-1);

    options[0].focus();
    await userEvent.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(options[1]);
    await userEvent.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(options[2]);

    // Going past the end stays on the last row.
    await userEvent.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(options[2]);

    await userEvent.keyboard("{Enter}");
    expect(onSelect).toHaveBeenCalledWith("c");

    await userEvent.keyboard("{Home}");
    expect(document.activeElement).toBe(options[0]);
    await userEvent.keyboard("{ArrowUp}");
    expect(document.activeElement).toBe(options[0]);
  });

  it("selects a row on click", async () => {
    const onSelect = vi.fn();
    render(
      <MeetingList
        meetings={[row("a", "Only one", new Date())]}
        recording={null}
        selectedId={null}
        onSelect={onSelect}
      />,
    );
    await userEvent.click(screen.getByRole("option", { name: /Only one/ }));
    expect(onSelect).toHaveBeenCalledWith("a");
  });
});
