import { act, fireEvent, render, screen } from "@testing-library/react";
import { createRef, useRef } from "react";
import { describe, expect, it } from "vitest";
import type { TranscriptSegment } from "../TranscriptPanel";
import TranscriptTab, { findSegmentIndex, type TranscriptTabHandle } from "./TranscriptTab";

/** A fake transcript: 0–5 s, 5–9 s and 9–12 s, with names already resolved. */
const segments: TranscriptSegment[] = [
  { meeting_id: "m1", sequence: 0, start_offset_ms: 0, end_offset_ms: 5_000, text: "shall we start", speaker: "You" },
  { meeting_id: "m1", sequence: 1, start_offset_ms: 5_000, end_offset_ms: 9_000, text: "yes go ahead", speaker: "Others" },
  { meeting_id: "m1", sequence: 2, start_offset_ms: 9_000, end_offset_ms: 12_000, text: "crosstalk", speaker: null },
];

function lineFor(text: string): HTMLElement {
  const li = screen.getByText(text).closest("li");
  if (!li) throw new Error(`no line for ${text}`);
  return li;
}

describe("findSegmentIndex", () => {
  it("finds the segment whose span holds the time", () => {
    expect(findSegmentIndex(segments, 0)).toBe(0);
    expect(findSegmentIndex(segments, 4_999)).toBe(0);
    expect(findSegmentIndex(segments, 5_000)).toBe(1);
    expect(findSegmentIndex(segments, 11_000)).toBe(2);
  });

  // A gap between segments reads as the line before it, not as nothing.
  it("falls back to the nearest line that started before the time", () => {
    const gapped: TranscriptSegment[] = [
      { ...segments[0], end_offset_ms: 2_000 },
      { ...segments[1], start_offset_ms: 6_000 },
    ];
    expect(findSegmentIndex(gapped, 4_000)).toBe(0);
    expect(findSegmentIndex(gapped, 60_000)).toBe(1);
  });

  it("returns -1 before anything has started", () => {
    expect(findSegmentIndex(segments.slice(1), 1_000)).toBe(-1);
    expect(findSegmentIndex([], 1_000)).toBe(-1);
  });
});

describe("TranscriptTab", () => {
  it("labels each transcript line with its speaker", () => {
    render(<TranscriptTab segments={segments} live={false} emptyMessage="none" />);
    expect(screen.getByText("shall we start")).toBeTruthy();
    expect(screen.getByText("You")).toBeTruthy();
    expect(screen.getByText("Others")).toBeTruthy();
  });

  // An ambiguous segment reads as unattributed rather than being pushed onto
  // whichever side was marginally louder.
  it("leaves a mixed segment unlabelled", () => {
    render(<TranscriptTab segments={segments} live={false} emptyMessage="none" />);
    expect(lineFor("crosstalk").textContent).toBe("[00:09]crosstalk");
  });

  it("shows the empty message when there are no segments", () => {
    render(<TranscriptTab segments={[]} live={false} emptyMessage="No segments recorded for this meeting." />);
    expect(screen.getByText("No segments recorded for this meeting.")).toBeTruthy();
  });

  // --- jumpTo: the handle the Ask AI citations call ---

  it("scrolls to and highlights the segment covering a time", () => {
    const ref = createRef<TranscriptTabHandle>();
    render(<TranscriptTab ref={ref} segments={segments} live={false} emptyMessage="none" />);

    expect(lineFor("yes go ahead").getAttribute("data-highlighted")).toBeNull();
    let found = false;
    act(() => {
      found = ref.current?.jumpTo(7_000) ?? false;
    });
    expect(found).toBe(true);
    expect(lineFor("yes go ahead").getAttribute("data-highlighted")).toBe("true");
    expect(lineFor("shall we start").getAttribute("data-highlighted")).toBeNull();
  });

  it("moves the highlight when another time is jumped to", () => {
    const ref = createRef<TranscriptTabHandle>();
    render(<TranscriptTab ref={ref} segments={segments} live={false} emptyMessage="none" />);
    act(() => {
      ref.current?.jumpTo(1_000);
    });
    act(() => {
      ref.current?.jumpTo(10_000);
    });
    expect(lineFor("crosstalk").getAttribute("data-highlighted")).toBe("true");
    expect(lineFor("shall we start").getAttribute("data-highlighted")).toBeNull();
  });

  it("reports a time before the first segment as not found, and highlights nothing", () => {
    const ref = createRef<TranscriptTabHandle>();
    render(<TranscriptTab ref={ref} segments={segments.slice(1)} live={false} emptyMessage="none" />);
    let found = true;
    act(() => {
      found = ref.current?.jumpTo(100) ?? true;
    });
    expect(found).toBe(false);
    expect(document.querySelector('[data-highlighted="true"]')).toBeNull();
  });

  // --- live follow ---

  /** A scroll container the tab follows, standing in for the pane body. */
  function Live({ rows, live }: { rows: TranscriptSegment[]; live: boolean }) {
    const body = useRef<HTMLDivElement>(null);
    return (
      <div ref={body} data-testid="body">
        <TranscriptTab segments={rows} live={live} emptyMessage="none" scrollRef={body} />
      </div>
    );
  }

  /** Gives an element the geometry a real scroll container would report. */
  function geometry(el: HTMLElement, scrollHeight: number, clientHeight: number, scrollTop: number) {
    Object.defineProperty(el, "scrollHeight", { configurable: true, value: scrollHeight });
    Object.defineProperty(el, "clientHeight", { configurable: true, value: clientHeight });
    Object.defineProperty(el, "scrollTop", { configurable: true, writable: true, value: scrollTop });
  }

  it("follows new lines while the live transcript is at the bottom", () => {
    const { rerender } = render(<Live rows={segments.slice(0, 1)} live />);
    const body = screen.getByTestId("body");
    geometry(body, 1_000, 200, 800);

    rerender(<Live rows={segments} live />);
    expect(body.scrollTop).toBe(1_000);
  });

  // Once the reader scrolls up to look at something, new lines must not drag
  // the view back down.
  it("stops following when the reader scrolls up, and resumes at the bottom", () => {
    const { rerender } = render(<Live rows={segments.slice(0, 1)} live />);
    const body = screen.getByTestId("body");
    geometry(body, 1_000, 200, 100);
    fireEvent.scroll(body);

    rerender(<Live rows={segments.slice(0, 2)} live />);
    expect(body.scrollTop).toBe(100);

    // Back at the bottom again: following resumes.
    geometry(body, 1_000, 200, 800);
    fireEvent.scroll(body);
    rerender(<Live rows={segments} live />);
    expect(body.scrollTop).toBe(1_000);
  });

  it("does not follow a meeting that is no longer recording", () => {
    const { rerender } = render(<Live rows={segments.slice(0, 1)} live={false} />);
    const body = screen.getByTestId("body");
    geometry(body, 1_000, 200, 0);
    rerender(<Live rows={segments} live={false} />);
    expect(body.scrollTop).toBe(0);
  });
});
