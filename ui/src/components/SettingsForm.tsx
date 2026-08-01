import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  REWRITE_MODES,
  deletePreset,
  getPromptOverride,
  getSetting,
  listPresets,
  setPromptOverride,
  setSetting,
  upsertPreset,
  type RewriteMode,
  type RewritePreset,
} from "../api";
import { Row, RowGroup } from "./SettingsRow";
import Spinner from "./Spinner";

export type RewriteSettings = {
  mode: RewriteMode;
  preset_id: string | null;
  custom_instruction: string;
};

type Props = {
  onChange?: (settings: RewriteSettings) => void;
  /** Rows rendered above the rewrite options in the same group (the hotkey). */
  leadingRows?: ReactNode;
};

const defaultMode: RewriteMode = "improve";

export default function SettingsForm({ onChange, leadingRows }: Props) {
  const [mode, setMode] = useState<RewriteMode>(defaultMode);
  const [presetId, setPresetId] = useState<string>("");
  const [customInstruction, setCustomInstruction] = useState("");
  const [promptOverride, setPromptOverrideText] = useState("");
  const [presets, setPresets] = useState<RewritePreset[]>([]);
  const [newPresetName, setNewPresetName] = useState("");
  const [newPresetInstruction, setNewPresetInstruction] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const loadedRef = useRef(false);
  const userInteractedRef = useRef(false);
  const pendingPersistRef = useRef<{ m: RewriteMode; pid: string; ci: string } | null>(null);

  const loadPresets = () =>
    listPresets()
      .then(setPresets)
      .catch((e) => setStatus(e instanceof Error ? e.message : String(e)));

  const doPersist = useCallback(
    (m: RewriteMode, pid: string, ci: string) => {
      setBusy(true);
      setStatus(null);
      Promise.all([
        setSetting("rewrite.active_mode", m),
        setSetting("rewrite.active_preset_id", pid),
        setSetting("rewrite.custom_instruction", ci),
      ])
        .then(() => setStatus("Rewrite settings saved."))
        .catch((e) => setStatus(e instanceof Error ? e.message : String(e)))
        .finally(() => setBusy(false));
    },
    [],
  );

  const persist = useCallback(
    (m: RewriteMode, pid: string, ci: string) => {
      userInteractedRef.current = true;
      if (!loadedRef.current) {
        pendingPersistRef.current = { m, pid, ci };
        return;
      }
      doPersist(m, pid, ci);
    },
    [doPersist],
  );

  useEffect(() => {
    Promise.all([
      getSetting("rewrite.active_mode"),
      getSetting("rewrite.active_preset_id"),
      getSetting("rewrite.custom_instruction"),
      loadPresets(),
    ])
      .then(([activeMode, activePreset, customInst]) => {
        if (userInteractedRef.current) {
          loadedRef.current = true;
          const pending = pendingPersistRef.current;
          if (pending) {
            doPersist(pending.m, pending.pid, pending.ci);
            pendingPersistRef.current = null;
          }
          return;
        }
        if (activeMode && REWRITE_MODES.some((m) => m.value === activeMode)) {
          setMode(activeMode as RewriteMode);
        }
        setPresetId(activePreset ?? "");
        setCustomInstruction(customInst ?? "");
        loadedRef.current = true;
      })
      .catch((e) => setStatus(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, [doPersist]);

  useEffect(() => {
    getPromptOverride(mode)
      .then((value) => setPromptOverrideText(value ?? ""))
      .catch((e) => setStatus(e instanceof Error ? e.message : String(e)));
  }, [mode]);

  useEffect(() => {
    onChange?.({
      mode,
      preset_id: presetId || null,
      custom_instruction: customInstruction,
    });
  }, [mode, presetId, customInstruction, onChange]);

  const savePromptOverride = async () => {
    setBusy(true);
    setStatus(null);
    try {
      await setPromptOverride(mode, promptOverride);
      setStatus("Prompt override saved.");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const addPreset = async () => {
    if (!newPresetName.trim() || !newPresetInstruction.trim()) return;
    setBusy(true);
    setStatus(null);
    try {
      const id = `preset-${Date.now()}`;
      await upsertPreset({
        id,
        name: newPresetName.trim(),
        instruction: newPresetInstruction.trim(),
      });
      setNewPresetName("");
      setNewPresetInstruction("");
      await loadPresets();
      setPresetId(id);
      persist(mode, id, customInstruction);
      setStatus("Preset added.");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const removePreset = async (id: string) => {
    setBusy(true);
    setStatus(null);
    try {
      await deletePreset(id);
      if (presetId === id) {
        setPresetId("");
        persist(mode, "", customInstruction);
      }
      await loadPresets();
      setStatus("Preset deleted.");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      {loading ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 120 }}>
          <Spinner size={16} />
          <span className="kea-muted">Loading settings…</span>
        </div>
      ) : (
        <>
          <RowGroup aria-label="Rewrite behavior">
            {leadingRows}
            <Row label="Style" hint="How KEA rewrites the text you select.">
              <select
                className="kea-select"
                aria-label="Rewrite style"
                value={mode}
                onChange={(e) => {
                  const m = e.target.value as RewriteMode;
                  setMode(m);
                  persist(m, presetId, customInstruction);
                }}
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
                onChange={(e) => {
                  const pid = e.target.value;
                  setPresetId(pid);
                  persist(mode, pid, customInstruction);
                }}
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
                  onChange={(e) => {
                    // Typing counts as interaction so a slow mount fetch can't
                    // clobber text entered before it resolves (persist is on blur).
                    userInteractedRef.current = true;
                    setCustomInstruction(e.target.value);
                  }}
                  onBlur={(e) => persist(mode, presetId, e.target.value)}
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
                  onChange={(e) => setPromptOverrideText(e.target.value)}
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
