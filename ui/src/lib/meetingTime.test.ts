import { describe, expect, it } from "vitest";
import { formatDuration, formatElapsed, meetingGroupFor } from "./meetingTime";

describe("meetingGroupFor", () => {
  // Friday 23 October 2026, mid-morning, local time.
  const now = new Date(2026, 9, 23, 10, 0);

  it("puts a meeting from today, yesterday, this week and before in their groups", () => {
    expect(meetingGroupFor(new Date(2026, 9, 23, 8, 0), now)).toBe("Today");
    expect(meetingGroupFor(new Date(2026, 9, 22, 18, 0), now)).toBe("Yesterday");
    // Monday 19 October starts this week; Sunday 18 October does not.
    expect(meetingGroupFor(new Date(2026, 9, 19, 9, 0), now)).toBe("This week");
    expect(meetingGroupFor(new Date(2026, 9, 18, 23, 59), now)).toBe("Earlier");
  });

  // A clock that is a little behind the meeting must not push it to Earlier.
  it("counts a meeting stamped after now as today", () => {
    expect(meetingGroupFor(new Date(2026, 9, 23, 10, 5), now)).toBe("Today");
  });

  it("starts the week on Monday, so a Monday morning has no This week", () => {
    const monday = new Date(2026, 9, 19, 9, 0);
    expect(meetingGroupFor(new Date(2026, 9, 18, 9, 0), monday)).toBe("Yesterday"); // Sunday
    expect(meetingGroupFor(new Date(2026, 9, 17, 9, 0), monday)).toBe("Earlier"); // Saturday
  });
});

describe("formatDuration", () => {
  it("reads short spans in seconds, then minutes, then hours", () => {
    expect(formatDuration(42_000)).toBe("42 sec");
    expect(formatDuration(7 * 60_000)).toBe("7 min");
    expect(formatDuration((62 * 60 + 5) * 1000)).toBe("1 h 02 min");
  });
});

describe("formatElapsed", () => {
  it("shows a running clock, with hours only once they pass", () => {
    expect(formatElapsed(42_000)).toBe("00:42");
    expect(formatElapsed(3_723_000)).toBe("1:02:03");
    expect(formatElapsed(-5)).toBe("00:00");
  });
});
