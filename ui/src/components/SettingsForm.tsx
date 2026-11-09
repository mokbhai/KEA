import { useState, type ReactNode } from "react";
import { TRANSLATION_TARGETS, isBuiltinStyle } from "../api";
import type { RewriteSettingsController } from "../hooks/useRewriteSettings";
import CustomStyles from "./CustomStyles";
import LoadingBlock from "./LoadingBlock";
import { Row, RowGroup } from "./SettingsRow";
import StyleOptions from "./StyleOptions";

/**
 * The Style menu entry that opens the "add a style" form instead of choosing
 * anything. Not a style id: those are `style-<time>` or a built-in tag.
 */
const NEW_STYLE = "__new_style__";

type Props = {
  /** The rewrite state, owned by whoever also runs the rewrites with it. */
  rewrite: RewriteSettingsController;
  /** Rows rendered above the rewrite options in the same group (the hotkey). */
  leadingRows?: ReactNode;
};

/**
 * The rewrite behaviour form. Presentational: every value it shows and every
 * write it makes goes through `rewrite`, so the only state left here is the
 * counter that asks "Your styles" to open its add form, which nothing else wants.
 */
export default function SettingsForm({ rewrite, leadingRows }: Props) {
  const [newStyleRequest, setNewStyleRequest] = useState(0);
  const { styles, promptOverride, loading, busy, status } = rewrite;
  const {
    style,
    custom_instruction: customInstruction,
    translate_target: translateTarget,
  } = rewrite.settings;
  // The two rows below belong to a built-in style's parameter; a custom style
  // carries its whole prompt and has none.
  const mode = isBuiltinStyle(style) ? style : null;

  const savePromptOverride = () => void rewrite.savePromptOverride();

  return (
    <>
      {loading ? (
        <LoadingBlock label="Loading settings…" minHeight={120} />
      ) : (
        <>
          <RowGroup aria-label="Rewrite behavior">
            {leadingRows}
            <Row label="Style" hint="How KEA rewrites the text you select.">
              <select
                className="kea-select"
                aria-label="Rewrite style"
                value={style}
                onChange={(e) => {
                  if (e.target.value === NEW_STYLE) {
                    // Opens the form; the selection itself does not change.
                    setNewStyleRequest((n) => n + 1);
                    return;
                  }
                  rewrite.chooseStyle(e.target.value);
                }}
              >
                <StyleOptions styles={styles} />
                <option value={NEW_STYLE}>+ New style…</option>
              </select>
            </Row>
            {mode === "translate" && (
              <Row
                label="Translate into"
                hint="The language the selection is rewritten in."
              >
                <select
                  className="kea-select"
                  aria-label="Target language"
                  value={translateTarget}
                  onChange={(e) => rewrite.chooseTranslateTarget(e.target.value)}
                >
                  {/* A tag inherited from the system may not be one we list;
                      showing it keeps the picker from silently re-targeting. */}
                  {!TRANSLATION_TARGETS.some((t) => t.tag === translateTarget) && (
                    <option value={translateTarget}>{translateTarget}</option>
                  )}
                  {TRANSLATION_TARGETS.map((t) => (
                    <option key={t.tag} value={t.tag}>
                      {t.label}
                    </option>
                  ))}
                </select>
              </Row>
            )}
            {mode === "ask_kea" && (
              <Row label="Instruction" hint="What Ask KEA should do with the selection.">
                <textarea
                  className="kea-input"
                  aria-label="Custom instruction"
                  value={customInstruction}
                  onChange={(e) => rewrite.editCustomInstruction(e.target.value)}
                  onBlur={(e) => rewrite.commitCustomInstruction(e.target.value)}
                  rows={2}
                  style={{ width: 280, resize: "vertical" }}
                  placeholder="Tell KEA what to do with the selection…"
                />
              </Row>
            )}
          </RowGroup>

          <CustomStyles rewrite={rewrite} newStyleRequest={newStyleRequest} />

          <details className="kea-advanced">
            <summary>Advanced</summary>
            <div className="kea-advanced__body">
              {mode ? (
                <>
                  <label>
                    <span className="kea-label">Prompt override (optional)</span>
                    <textarea
                      className="kea-input"
                      value={promptOverride}
                      onChange={(e) => rewrite.editPromptOverride(e.target.value)}
                      rows={3}
                      style={{ resize: "vertical" }}
                      placeholder="Override the built-in prompt for the selected style"
                    />
                  </label>
                  {mode === "translate" && (
                    <p className="kea-muted" style={{ margin: 0, fontSize: "0.8125rem" }}>
                      One prompt covers every language: keep{" "}
                      <code>{"{{target_language}}"}</code> in it, which is replaced with
                      the language you picked.
                    </p>
                  )}
                  <div>
                    <button
                      type="button"
                      className="kea-btn"
                      onClick={savePromptOverride}
                      disabled={busy}
                    >
                      Save prompt override
                    </button>
                  </div>
                </>
              ) : (
                <p className="kea-muted" style={{ margin: 0, fontSize: "0.8125rem" }}>
                  The selected style is one of yours, so its instruction is the whole
                  prompt. Edit it under Your styles.
                </p>
              )}
            </div>
          </details>

          {status && (
            <p className="kea-muted" style={{ marginTop: 8, marginBottom: 0 }}>
              {status}
            </p>
          )}
        </>
      )}
    </>
  );
}
