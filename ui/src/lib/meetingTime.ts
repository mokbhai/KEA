/**
 * Time helpers for the meetings list.
 *
 * Everything here works in the user's local time zone: a meeting that started
 * at 23:50 last night is "Yesterday" on the screen whatever the UTC date is.
 */

export type MeetingGroupLabel = "Today" | "Yesterday" | "This week" | "Earlier";

export const MEETING_GROUP_ORDER: readonly MeetingGroupLabel[] = [
  "Today",
  "Yesterday",
  "This week",
  "Earlier",
];

const DAY_MS = 24 * 60 * 60 * 1000;

/** `YYYY-MM-DD HH:MM[:SS[.fff]]` with no zone: SQLite's `datetime('now')`. */
const ZONELESS = /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}(:\d{2}(\.\d+)?)?$/;

/**
 * A stored meeting timestamp as epoch milliseconds, or NaN.
 *
 * The database writes UTC with no zone suffix (`2026-10-22 20:37:14`), and
 * `Date.parse` reads a zoneless string as *local* time — which put every
 * meeting off by the user's UTC offset: a meeting recorded at 02:07 in
 * India showed as yesterday at 20:37, and a live recording's clock started
 * at 5:30:00. A string with an explicit zone is parsed as given.
 */
export function parseStoredTime(value: string | null | undefined): number {
  if (!value) return NaN;
  const trimmed = value.trim();
  return Date.parse(ZONELESS.test(trimmed) ? `${trimmed.replace(" ", "T")}Z` : trimmed);
}

/** A stored timestamp as a local date and time, e.g. "23 Oct, 02:07"; the raw text if unreadable. */
export function formatStoredDateTime(value: string): string {
  const ms = parseStoredTime(value);
  if (Number.isNaN(ms)) return value;
  return new Date(ms).toLocaleString([], {
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Local midnight at the start of the day `d` falls on. */
function startOfLocalDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

/**
 * Which group a meeting started at `startedAt` belongs under, as seen at `now`.
 *
 * "This week" is the calendar week from Monday, excluding yesterday, which has
 * its own group. A meeting stamped after `now` (clock skew) counts as today.
 */
export function meetingGroupFor(startedAt: Date, now: Date): MeetingGroupLabel {
  const today = startOfLocalDay(now);
  const day = startOfLocalDay(startedAt);
  if (day.getTime() >= today.getTime()) return "Today";

  // Calendar days, not 24-hour spans, so a DST change cannot shift a meeting
  // into the wrong group.
  const daysAgo = Math.round((today.getTime() - day.getTime()) / DAY_MS);
  if (daysAgo === 1) return "Yesterday";

  const mondayOffset = (today.getDay() + 6) % 7;
  const monday = new Date(today.getFullYear(), today.getMonth(), today.getDate() - mondayOffset);
  if (day.getTime() >= monday.getTime()) return "This week";

  return "Earlier";
}

/** Two-digit clock time in local time, e.g. "09:05". */
export function formatClock(d: Date): string {
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/**
 * A length of time as a short label: "42 sec", "7 min", "1 h 05 min".
 * Seconds are dropped past a minute so a row stays scannable.
 */
export function formatDuration(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  if (totalSec < 60) return `${totalSec} sec`;
  const totalMin = Math.floor(totalSec / 60);
  if (totalMin < 60) return `${totalMin} min`;
  const hours = Math.floor(totalMin / 60);
  const mins = totalMin % 60;
  return `${hours} h ${String(mins).padStart(2, "0")} min`;
}

/**
 * The running clock shown beside a recording: "00:42", or "1:02:09" past an
 * hour.
 */
export function formatElapsed(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}
