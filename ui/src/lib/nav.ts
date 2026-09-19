/**
 * The settings window's routes, in one place.
 *
 * Three overlapping unions used to spell subsets of this list — the shell's
 * own `Page`, the banner's navigate callback, and a `FixTarget` returned from
 * the slot diagnosis — so a renamed route only failed to compile in whichever
 * copy happened to mention it. Everything that navigates names this type.
 */
export type Page =
  | "rewrite"
  | "dictation"
  | "meetings"
  | "transcribe"
  | "read-aloud"
  | "ai-providers"
  | "models"
  | "vocabulary"
  | "profiles"
  | "general"
  | "history"
  | "usage"
  | "logs";

/**
 * The same routes as values, for the places that need to check one at runtime
 * rather than at compile time — a `kea://open/<page>` URL arrives as a string.
 * Kept beside the union so a new route cannot be added to one and not the
 * other: the type annotation makes a missing entry a compile error.
 */
export const PAGES: readonly Page[] = [
  "rewrite",
  "dictation",
  "meetings",
  "transcribe",
  "read-aloud",
  "ai-providers",
  "models",
  "vocabulary",
  "profiles",
  "general",
  "history",
  "usage",
  "logs",
];

/** Whether an untrusted string names a settings page. */
export function isPage(value: string): value is Page {
  return (PAGES as readonly string[]).includes(value);
}

/** Switches the settings window to another page. */
export type Navigate = (page: Page) => void;
