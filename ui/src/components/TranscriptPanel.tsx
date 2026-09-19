import { useEffect, useRef } from "react";

export type TranscriptSegment = {
  meeting_id: string;
  sequence: number;
  start_offset_ms: number;
  end_offset_ms: number;
  text: string;
  /**
   * Who spoke, already resolved to a display name. Absent means "unknown",
   * which renders as no chip at all — exactly how every transcript looked
   * before diarization existed.
   */
  speaker?: string | null;
};

type Props = {
  segments: TranscriptSegment[];
  live?: boolean;
  emptyMessage?: string;
};

function formatOffset(ms: number): string {
  const totalSec = Math.floor(ms / 1000);
  const min = Math.floor(totalSec / 60);
  const sec = totalSec % 60;
  return `${String(min).padStart(2, "0")}:${String(sec).padStart(2, "0")}`;
}

export default function TranscriptPanel({
  segments,
  live = false,
  emptyMessage = "No transcript yet.",
}: Props) {
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (live && bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: "smooth" });
    }
  }, [segments, live]);

  return (
    <div
      style={{
        border: "1px solid var(--border)",
        borderRadius: 8,
        background: "var(--surface-2)",
        maxHeight: 280,
        overflowY: "auto",
        padding: 12,
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
        fontSize: 13,
        lineHeight: 1.5,
      }}
    >
      {segments.length === 0 ? (
        <p className="kea-muted" style={{ margin: 0 }}>
          {emptyMessage}
        </p>
      ) : (
        <ul style={{ margin: 0, padding: 0, listStyle: "none" }}>
          {segments.map((seg) => (
            <li
              // Composite, not `seg.sequence`: a sequence is unique per
              // segment but not per *turn*, and splitting a segment at a
              // speaker change would otherwise make React drop rows.
              key={`${seg.sequence}-${seg.start_offset_ms}`}
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
          ))}
        </ul>
      )}
      <div ref={bottomRef} />
    </div>
  );
}
