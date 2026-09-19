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
  | "read-aloud"
  | "ai-providers"
  | "models"
  | "general"
  | "history"
  | "logs";

/** Switches the settings window to another page. */
export type Navigate = (page: Page) => void;
