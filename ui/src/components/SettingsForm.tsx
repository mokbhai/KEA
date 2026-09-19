import { useState, type ReactNode } from "react";
import {
  REWRITE_MODES,
  TRANSLATION_TARGETS,
  type RewriteMode,
  type RewritePreset,
} from "../api";
import { useLlmChoices } from "../hooks/useLlmChoices";
import type { RewriteSettingsController } from "../hooks/useRewriteSettings";
import LoadingBlock from "./LoadingBlock";
import { Row, RowGroup } from "./SettingsRow";

/**
 * The `<select>` value standing for "use the Rewrite AI", stored as null.
 * The empty string, because no engine or provider id can be empty — the same
 * spelling the app-rule editor uses for the same idea.
 */
const INHERIT = "";

const orNull = (value: string) => (value.trim() === "" ? null : value.trim());

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
  const {
    mode,
    custom_instruction: customInstruction,
    translate_target: translateTarget,
  } = rewrite.settings;
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
                      <li key={p.id} style={{ marginBottom: 12 }}>
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
                        <PresetAi
                          preset={p}
                          busy={busy}
                          onSave={(next) => void rewrite.savePreset(next)}
                        />
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

type PresetAiProps = {
  preset: RewritePreset;
  busy: boolean;
  onSave: (preset: RewritePreset) => void;
};

/**
 * The AI one preset asks for.
 *
 * Bindings are per capability slot, so without this every preset shares one
 * model — you cannot spend a cheap model on grammar and an expensive one on a
 * hard rewrite. Leaving the engine on "the Rewrite AI" ignores the other two
 * fields, exactly as an app rule does: a model with no engine names nothing
 * KEA can resolve.
 */
function PresetAi({ preset, busy, onSave }: PresetAiProps) {
  const { engines, providers } = useLlmChoices();
  const overridden = preset.llm_engine_id !== null;

  return (
    <details style={{ marginTop: 4 }}>
      <summary style={{ cursor: "pointer", fontSize: "0.8125rem" }}>
        AI: {overridden ? (preset.llm_model ?? preset.llm_engine_id) : "the Rewrite AI"}
      </summary>
      <div style={{ display: "grid", gap: 8, padding: "8px 0 0 4px" }}>
        <p className="kea-muted" style={{ margin: 0, fontSize: "0.8125rem" }}>
          An app rule that names its own AI wins over this one — a rule about where
          the text is going is a constraint, and this is a preference about the job.
        </p>
        <label>
          <span className="kea-label">AI engine</span>
          <select
            className="kea-select"
            aria-label={`AI engine for ${preset.name}`}
            value={preset.llm_engine_id ?? INHERIT}
            disabled={busy}
            onChange={(e) =>
              onSave({ ...preset, llm_engine_id: orNull(e.target.value) })
            }
          >
            <option value={INHERIT}>The Rewrite AI</option>
            {engines.map((id) => (
              <option key={id} value={id}>
                {id}
              </option>
            ))}
            {/* An engine saved by a newer build still has to show as the
                selection, or this dropdown would silently re-target it. */}
            {preset.llm_engine_id && !engines.includes(preset.llm_engine_id) && (
              <option value={preset.llm_engine_id}>{preset.llm_engine_id}</option>
            )}
          </select>
        </label>
        <label>
          <span className="kea-label">Model</span>
          <input
            className="kea-input"
            aria-label={`Model for ${preset.name}`}
            value={preset.llm_model ?? ""}
            disabled={busy || !overridden}
            onChange={(e) => onSave({ ...preset, llm_model: orNull(e.target.value) })}
            placeholder="gpt-4o-mini"
          />
        </label>
        <label>
          <span className="kea-label">Provider</span>
          <select
            className="kea-select"
            aria-label={`Provider for ${preset.name}`}
            value={preset.llm_provider_ref ?? INHERIT}
            disabled={busy || !overridden}
            onChange={(e) =>
              onSave({ ...preset, llm_provider_ref: orNull(e.target.value) })
            }
          >
            <option value={INHERIT}>The engine's own</option>
            {providers.map((p) => (
              <option key={p.provider_ref} value={p.provider_ref}>
                {p.name}
              </option>
            ))}
          </select>
        </label>
      </div>
    </details>
  );
}
