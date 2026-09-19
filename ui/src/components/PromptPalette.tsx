import { useCallback, useEffect, useRef, useState } from "react";
import {
  cancelPalette,
  getPaletteSession,
  listPaletteHistory,
  onPaletteClose,
  onPaletteOpen,
  paletteReady,
  runPalette,
  type PaletteDelivery,
  type PaletteSession,
} from "../api";
import { toMessage } from "../lib/format";
import Spinner from "./Spinner";

/** How much of the source is worth showing. Beyond this it is scenery. */
const PREVIEW_CHARS = 220;

/**
 * The draft's slot in the history walk. Arrow-up counts 0, 1, 2… into the
 * history; -1 is "back at what I was typing".
 */
const DRAFT = -1;

function preview(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  // `Array.from`, never `slice`: a byte or UTF-16 cut lands inside an emoji or
  // a CJK character and renders a replacement glyph.
  const scalars = Array.from(flat);
  return scalars.length <= PREVIEW_CHARS
    ? flat
    : `${scalars.slice(0, PREVIEW_CHARS).join("")}…`;
}

/**
 * Which delivery a submit means.
 *
 * The key *is* the choice, which is what saves a second focus round trip: the
 * palette would otherwise have to show the result, ask where to put it, and
 * hand focus back afterwards. `Insert` collapses to `Copy` when KEA cannot
 * type at all (no Accessibility, or secure input), because offering a key that
 * silently does nothing is worse than offering fewer keys.
 */
export function deliveryForKey(
  event: { key: string; metaKey: boolean; ctrlKey: boolean; shiftKey: boolean },
  session: Pick<PaletteSession, "can_insert" | "default_delivery">,
): PaletteDelivery | null {
  if (event.key !== "Enter") return null;
  const command = event.metaKey || event.ctrlKey;
  if (command && event.shiftKey) return "copy";
  if (command) return session.can_insert ? "insert" : "copy";
  return session.default_delivery;
}

const deliveryLabels: Record<PaletteDelivery, string> = {
  replace: "Replace the selection",
  insert: "Insert at the cursor",
  copy: "Copy to the clipboard",
};

/**
 * The prompt palette: the one KEA window that takes keyboard focus.
 *
 * It renders nothing at all with no session, which is its resting state — the
 * window itself is hidden by Rust, and a component that painted an empty card
 * would flash one every time the window is shown.
 *
 * Everything the backend had to do before this could appear (read the
 * selection, record the target app's pid) is documented in
 * `src-tauri/src/palette.rs`; nothing in here may assume the user's app is
 * still frontmost, because it is not.
 */
export default function PromptPalette() {
  const [session, setSession] = useState<PaletteSession | null>(null);
  const [instruction, setInstruction] = useState("");
  const [history, setHistory] = useState<string[]>([]);
  const [historyIndex, setHistoryIndex] = useState(DRAFT);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  /** The instruction the user was typing before they started walking history. */
  const draftRef = useRef("");

  const reset = useCallback(() => {
    setSession(null);
    setInstruction("");
    setHistoryIndex(DRAFT);
    setRunning(false);
    setError(null);
    draftRef.current = "";
  }, []);

  useEffect(() => {
    const unsubs = Promise.all([
      onPaletteOpen(async (sessionId) => {
        try {
          const next = await getPaletteSession(sessionId);
          setSession(next);
          setInstruction("");
          setHistoryIndex(DRAFT);
          setError(null);
          setRunning(false);
          draftRef.current = "";
          // Fetched per open rather than once at mount: it changes every time
          // the palette is used, and this window outlives every session.
          setHistory(await listPaletteHistory().catch(() => []));
          // This is what puts the window on screen. Until it lands the palette
          // is built but hidden, so it can never appear holding the previous
          // session's text.
          await paletteReady(sessionId);
        } catch (e) {
          // The session was dismissed between the event and the fetch. There
          // is no window to show the error in, so it is not an error.
          setError(toMessage(e));
        }
      }),
      onPaletteClose(reset),
    ]);
    return () => {
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, [reset]);

  useEffect(() => {
    // The window is shown after the session renders, so the focus call has to
    // come from the render that has one.
    if (session) inputRef.current?.focus();
  }, [session]);

  const dismiss = useCallback(() => {
    // Reset first: the backend hides the window, and a component still holding
    // the old session would repaint it for a frame on the next open.
    reset();
    void cancelPalette().catch(() => {});
  }, [reset]);

  /**
   * Walks the instruction history, remembering the half-typed draft.
   *
   * Losing an in-progress instruction to a stray arrow key is the bug this
   * component would otherwise ship with, so the draft is stashed on the way
   * out of it and restored on the way back.
   */
  const walkHistory = useCallback(
    (step: number) => {
      if (history.length === 0) return;
      setHistoryIndex((current) => {
        if (current === DRAFT && step > 0) draftRef.current = instruction;
        const next = Math.min(Math.max(current + step, DRAFT), history.length - 1);
        setInstruction(next === DRAFT ? draftRef.current : history[next]);
        return next;
      });
    },
    [history, instruction],
  );

  const submit = useCallback(
    async (delivery: PaletteDelivery) => {
      if (!session || running) return;
      const trimmed = instruction.trim();
      if (!trimmed) return;
      setRunning(true);
      setError(null);
      try {
        await runPalette(session.session_id, trimmed, delivery);
        // Nothing to do on success: the backend hides the window and emits
        // `palette:close`, which resets this component.
      } catch (e) {
        setError(toMessage(e));
        // Deliberately stays open with the instruction intact — an LLM that
        // refused or a provider that is not configured is worth one edit and
        // a second Return, not a retype.
        setRunning(false);
      }
    },
    [session, running, instruction],
  );

  if (!session) return null;

  const hasSource = session.source_text.trim().length > 0;

  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      dismiss();
      return;
    }
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      walkHistory(event.key === "ArrowUp" ? 1 : -1);
      return;
    }
    const delivery = deliveryForKey(event, session);
    if (delivery) {
      event.preventDefault();
      void submit(delivery);
    }
  };

  return (
    <div className="kea-palette" role="dialog" aria-label="Ask KEA" style={cardStyle}>
      <div>
        {session.origin === "screen_capture" && (
          <span style={badgeStyle}>From screen capture</span>
        )}
        {hasSource ? (
          <p style={sourceStyle} aria-label="Selected text">
            {preview(session.source_text)}
          </p>
        ) : (
          <p className="kea-muted" style={{ margin: 0 }}>
            Ask KEA anything
            {session.app_name ? ` — nothing selected in ${session.app_name}` : ""}
          </p>
        )}
      </div>

      <label className="kea-visually-hidden" htmlFor="kea-palette-input">
        Instruction
      </label>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <input
          id="kea-palette-input"
          ref={inputRef}
          className="kea-input"
          type="text"
          autoComplete="off"
          spellCheck={false}
          placeholder={hasSource ? "What should KEA do with this?" : "Ask KEA…"}
          value={instruction}
          disabled={running}
          onChange={(e) => {
            setInstruction(e.target.value);
            // Typing leaves the history walk; the edit becomes the draft.
            setHistoryIndex(DRAFT);
            draftRef.current = e.target.value;
          }}
          onKeyDown={onKeyDown}
          style={{ flex: 1 }}
        />
        {running && <Spinner size={16} />}
      </div>

      {session.notice && (
        <p className="kea-muted" style={noticeStyle}>
          {session.notice}
        </p>
      )}
      {error && (
        <p style={errorStyle} role="alert">
          {error}
        </p>
      )}

      <div className="kea-muted" style={hintRowStyle}>
        <span>↩ {deliveryLabels[session.default_delivery]}</span>
        {session.can_insert && session.default_delivery !== "insert" && (
          <span>⌘↩ {deliveryLabels.insert}</span>
        )}
        {session.default_delivery !== "copy" && (
          <span>⇧⌘↩ {deliveryLabels.copy}</span>
        )}
        <span>esc Cancel</span>
      </div>
    </div>
  );
}

/* All of it in design tokens, and none of it using `opacity`: the palette
   contrast guard in `ui/src/palette.contrast.test.ts` composites declared
   alpha only, so an `opacity` here would drop the real ratio below 4.5:1 with
   the test still green.

   Inline rather than in index.css because this window is transparent and the
   card *is* the window — there is no page around it whose stylesheet these
   rules would belong to, and every one of them is used exactly once. */
const cardStyle: React.CSSProperties = {
  boxSizing: "border-box",
  width: "100%",
  padding: 16,
  color: "var(--text)",
  background: "var(--surface)",
  border: "1px solid var(--border)",
  borderRadius: 12,
  boxShadow: "0 12px 32px rgba(0, 0, 0, 0.28)",
};

const badgeStyle: React.CSSProperties = {
  display: "inline-block",
  marginBottom: 8,
  padding: "2px 8px",
  fontSize: "0.75rem",
  color: "var(--text-muted)",
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  borderRadius: 999,
};

const sourceStyle: React.CSSProperties = {
  margin: 0,
  maxHeight: 72,
  overflow: "hidden",
  color: "var(--text-muted)",
  fontSize: "0.8125rem",
  lineHeight: 1.5,
};

const noticeStyle: React.CSSProperties = {
  margin: "8px 0 0",
  fontSize: "0.8125rem",
};

const errorStyle: React.CSSProperties = {
  margin: "8px 0 0",
  color: "var(--danger)",
  fontSize: "0.8125rem",
};

const hintRowStyle: React.CSSProperties = {
  display: "flex",
  flexWrap: "wrap",
  gap: 12,
  marginTop: 12,
  fontSize: "0.75rem",
};
