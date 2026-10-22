import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type RefObject,
} from "react";
import { formatOffset, type TranscriptSegment } from "../TranscriptPanel";

/** How long a jumped-to segment keeps its highlight. */
const HIGHLIGHT_MS = 2000;

/** How close to the bottom counts as "following" the live transcript. */
const FOLLOW_SLACK_PX = 32;

export type TranscriptTabHandle = {
  /**
   * Scroll to the segment that covers `ms` (or the nearest one that started
   * before it) and highlight it. Returns false when no segment is at or before
   * that time, so the caller can say so instead of silently doing nothing.
   */
  jumpTo: (ms: number) => boolean;
};

export type TranscriptTabProps = {
  segments: TranscriptSegment[];
  /** The meeting is recording: new lines follow the bottom unless scrolled up. */
  live: boolean;
  emptyMessage: string;
  /**
   * The element that scrolls the tab (the pane body). Following and scrolling
   * a segment into view both act on it. Absent, the tab does neither.
   */
  scrollRef?: RefObject<HTMLElement>;
};

/**
 * The index of the segment covering `ms`: the one whose `[start, end)` holds
 * it, else the last one that started at or before it. -1 when nothing started
 * yet. Segments are in order, so one pass is enough.
 */
export function findSegmentIndex(segments: TranscriptSegment[], ms: number): number {
  let nearest = -1;
  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i];
    if (ms >= seg.start_offset_ms && ms < seg.end_offset_ms) return i;
    if (seg.start_offset_ms <= ms) nearest = i;
  }
  return nearest;
}

/**
 * The timed, speaker-labelled transcript of a meeting.
 *
 * Renders from whatever `segments` it is given, so the same tab serves the
 * finished meeting and the live one. The `jumpTo` handle is what the Ask AI
 * citations will call.
 */
const TranscriptTab = forwardRef<TranscriptTabHandle, TranscriptTabProps>(function TranscriptTab(
  { segments, live, emptyMessage, scrollRef },
  ref,
) {
  const itemRefs = useRef(new Map<number, HTMLLIElement>());
  // Starts true: a live meeting opens at its newest line.
  const followingRef = useRef(true);
  const [highlight, setHighlight] = useState<{ index: number; token: number } | null>(null);

  // Scrolling up stops the follow; scrolling back to the bottom resumes it.
  useEffect(() => {
    const el = scrollRef?.current;
    if (!el) return;
    const onScroll = () => {
      followingRef.current =
        el.scrollHeight - el.scrollTop - el.clientHeight < FOLLOW_SLACK_PX;
    };
    el.addEventListener("scroll", onScroll);
    return () => el.removeEventListener("scroll", onScroll);
  }, [scrollRef]);

  // A tab coming back keeps the place it was left at: the pane restores the
  // scroll position before this runs, and anywhere but the bottom means the
  // reader had scrolled up, so the follow starts off.
  useEffect(() => {
    const el = scrollRef?.current;
    if (el && el.scrollTop > 0) {
      followingRef.current =
        el.scrollHeight - el.scrollTop - el.clientHeight < FOLLOW_SLACK_PX;
    }
    // Mount only: afterwards the scroll listener keeps it current.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Runs after mount and after every new line. Our own scroll then re-enters
  // the follow zone, which is what keeps the next line following too.
  useEffect(() => {
    if (!live) return;
    const el = scrollRef?.current;
    if (el && followingRef.current) el.scrollTop = el.scrollHeight;
  }, [segments, live, scrollRef]);

  useEffect(() => {
    if (!highlight) return;
    const timer = setTimeout(() => setHighlight(null), HIGHLIGHT_MS);
    return () => clearTimeout(timer);
  }, [highlight]);

  useImperativeHandle(
    ref,
    () => ({
      jumpTo: (ms: number) => {
        const index = findSegmentIndex(segments, ms);
        if (index < 0) return false;
        // The reader chose this line; new lines must not drag the view away.
        followingRef.current = false;
        itemRefs.current.get(index)?.scrollIntoView?.({ block: "center" });
        // A new token restarts the highlight even when it lands on the same line.
        setHighlight((prev) => ({ index, token: (prev?.token ?? 0) + 1 }));
        return true;
      },
    }),
    [segments],
  );

  if (segments.length === 0) {
    return (
      <p className="kea-muted" style={{ margin: 0 }}>
        {emptyMessage}
      </p>
    );
  }

  return (
    <ul style={{ margin: 0, padding: 0, listStyle: "none", fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace", fontSize: 13, lineHeight: 1.5 }}>
      {segments.map((seg, index) => {
        const highlighted = highlight?.index === index;
        return (
          <li
            // Composite, not `seg.sequence`: a sequence is unique per segment
            // but not per turn, and splitting a segment at a speaker change
            // would otherwise make React drop rows.
            key={`${seg.sequence}-${seg.start_offset_ms}`}
            ref={(el) => {
              if (el) itemRefs.current.set(index, el);
              else itemRefs.current.delete(index);
            }}
            data-highlighted={highlighted ? "true" : undefined}
            className={highlighted ? "kea-transcript-line kea-transcript-line--flash" : "kea-transcript-line"}
            style={{
              marginBottom: 10,
              paddingBottom: 10,
              borderBottom: "1px solid var(--border)",
            }}
          >
            <span style={{ color: "var(--text-muted)", marginRight: 8 }}>
              [{formatOffset(seg.start_offset_ms)}]
            </span>
            {seg.speaker && (
              <span
                style={{
                  marginRight: 8,
                  padding: "1px 6px",
                  borderRadius: 4,
                  border: "1px solid var(--border)",
                  background: "var(--surface)",
                  fontSize: 11,
                }}
              >
                {seg.speaker}
              </span>
            )}
            <span>{seg.text}</span>
          </li>
        );
      })}
    </ul>
  );
});

export default TranscriptTab;
