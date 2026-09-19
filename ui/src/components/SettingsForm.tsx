import { useState, type ReactNode } from "react";
import { REWRITE_MODES, type RewriteMode } from "../api";
import type { RewriteSettingsController } from "../hooks/useRewriteSettings";
import LoadingBlock from "./LoadingBlock";
import { Row, RowGroup } from "./SettingsRow";

type Props = {
  /** The rewrite state, owned by whoever also runs the rewrites with it. */
  rewrite: RewriteSettingsController;
  /** Rows rendered above the rewrite options in the same group (the hotkey). */
  leadingRows?: ReactNode;
};

/**
 * The rewrite behaviour form. Presentational: every value it shows and every
 * write it makes goes through `rewrite`, so the only state left here is the
 * two half-typed fields of the "add a preset" row, which nothing else wants.
 */
export default function SettingsForm({ rewrite, leadingRows }: Props) {
  const [newPresetName, setNewPresetName] = useState("");
  const [newPresetInstruction, setNewPresetInstruction] = useState("");
  const { presets, promptOverride, loading, busy, status, removePreset } = rewrite;
  const { mode, custom_instruction: customInstruction } = rewrite.settings;
  const presetId = rewrite.settings.preset_id ?? "";

  const savePromptOverride = () => void rewrite.savePromptOverride();

  const addPreset = async () => {
    if (await rewrite.addPreset(newPresetName, newPresetInstruction)) {
      setNewPresetName("");
      setNewPresetInstruction("");
    }
  };

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
                value={mode}
                onChange={(e) => rewrite.chooseMode(e.target.value as RewriteMode)}
              >
                {REWRITE_MODES.map((m) => (
                  <option key={m.value} value={m.value}>
                    {m.label}
                  </option>
                ))}
              </select>
            </Row>
            <Row label="Preset" hint="A saved instruction, used instead of the style.">
              <select
                className="kea-select"
                aria-label="Rewrite preset"
                value={presetId}
                onChange={(e) => rewrite.choosePreset(e.target.value)}
              >
                <option value="">None (use style)</option>
                {presets.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
            </Row>
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

          <details className="kea-advanced">
            <summary>Advanced</summary>
            <div className="kea-advanced__body">
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
              <fieldset
                style={{
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  padding: 12,
                  margin: 0,
                }}
              >
                <legend style={{ fontWeight: 600 }}>Manage presets</legend>
                <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 8 }}>
                  <input
                    className="kea-input"
                    value={newPresetName}
                    onChange={(e) => setNewPresetName(e.target.value)}
                    placeholder="Preset name"
                    aria-label="Preset name"
                    style={{ minWidth: 140, maxWidth: 200 }}
                  />
                  <input
                    className="kea-input"
                    value={newPresetInstruction}
                    onChange={(e) => setNewPresetInstruction(e.target.value)}
                    placeholder="Instruction"
                    aria-label="Preset instruction"
                    style={{ flex: 1, minWidth: 200, maxWidth: "none" }}
                  />
                  <button
                    type="button"
                    className="kea-btn"
                    onClick={addPreset}
                    disabled={busy || !newPresetName.trim() || !newPresetInstruction.trim()}
                  >
                    Add preset
                  </button>
                </div>
                {presets.length > 0 && (
                  <ul style={{ margin: 0, paddingLeft: 20 }}>
                    {presets.map((p) => (
                      <li key={p.id} style={{ marginBottom: 4 }}>
                        <strong>{p.name}</strong> — {p.instruction.slice(0, 60)}
                        {p.instruction.length > 60 ? "…" : ""}{" "}
                        <button
                          type="button"
                          className="kea-btn"
                          onClick={() => removePreset(p.id)}
                          disabled={busy}
                          style={{ marginLeft: 8 }}
                        >
                          Delete
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </fieldset>
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

