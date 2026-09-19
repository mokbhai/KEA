import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import TranscriptPanel, {
  speakerDisplayName,
  type MeetingSpeakerRow,
  type TranscriptSegment,
} from "./TranscriptPanel";

function segment(over: Partial<TranscriptSegment> = {}): TranscriptSegment {
  return {
    meeting_id: "m1",
    sequence: 0,
    start_offset_ms: 0,
    end_offset_ms: 5_000,
    text: "hello",
    ...over,
  };
}

const speakers: MeetingSpeakerRow[] = [
  { meeting_id: "m1", speaker_key: "local", display_name: "You", source: "channel" },
  { meeting_id: "m1", speaker_key: "remote", display_name: "Priya", source: "user" },
];

describe("speakerDisplayName", () => {
  it("falls back to the channel's built-in name when nothing is stored", () => {
    expect(speakerDisplayName("local")).toBe("You");
    expect(speakerDisplayName("remote")).toBe("Others");
  });

  it("prefers a name the user stored for this meeting", () => {
    expect(speakerDisplayName("remote", speakers)).toBe("Priya");
  });

  // "We could not tell" must not become a third speaker, or a coin flip
  // between the two real ones.
  it("says nothing for an ambiguous or missing key", () => {
    expect(speakerDisplayName("mixed")).toBeNull();
    expect(speakerDisplayName(null)).toBeNull();
    expect(speakerDisplayName(undefined)).toBeNull();
    expect(speakerDisplayName("spk7")).toBeNull();
  });
});

describe("TranscriptPanel", () => {
  it("shows a chip for an attributed line", () => {
    render(<TranscriptPanel segments={[segment({ speaker: "You" })]} />);
    expect(screen.getByText("You")).toBeTruthy();
    expect(screen.getByText("hello")).toBeTruthy();
  });

  // Exactly how every transcript looked before attribution existed.
  it("renders an unattributed line with no chip at all", () => {
    const { container } = render(
      <TranscriptPanel segments={[segment({ speaker: null })]} />,
    );
    expect(screen.getByText("hello")).toBeTruthy();
    expect(container.querySelectorAll("li span")).toHaveLength(2); // offset + text
  });

  // A sequence is unique per segment but not per turn; keying on it alone
  // would make React drop rows once a segment is split at a speaker change.
  it("keeps both rows when two turns share a sequence", () => {
    render(
      <TranscriptPanel
        segments={[
          segment({ sequence: 0, start_offset_ms: 0, text: "mine", speaker: "You" }),
          segment({
            sequence: 0,
            start_offset_ms: 2_000,
            text: "theirs",
            speaker: "Others",
          }),
        ]}
      />,
    );
    expect(screen.getByText("mine")).toBeTruthy();
    expect(screen.getByText("theirs")).toBeTruthy();
  });
});
