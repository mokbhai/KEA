import { useCallback, useEffect, useRef, useState } from "react";
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
import Spinner from "./Spinner";

export type RewriteSettings = {
  mode: RewriteMode;
  preset_id: string | null;
  custom_instruction: string;
};

type Props = {
  onChange?: (settings: RewriteSettings) => void;
};

const defaultMode: RewriteMode = "improve";

export default function SettingsForm({ onChange }: Props) {
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
    <section className="kea-card" style={{ marginBottom: 16 }}>
      <h3 style={{ margin: "0 0 12px" }}>Rewrite settings</h3>
      {loading ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 200 }}>
          <Spinner size={16} />
          <span className="kea-muted">Loading settings…</span>
        </div>
      ) : (
        <>
      <label style={{ display: "block", marginBottom: 12 }}>
        <span className="kea-label">Mode</span>
        <select
          className="kea-select"
          value={mode}
          onChange={(e) => {
            const m = e.target.value as RewriteMode;
            setMode(m);
            persist(m, presetId, customInstruction);
          }}
          style={{ minWidth: 220 }}
        >
          {REWRITE_MODES.map((m) => (
            <option key={m.value} value={m.value}>
              {m.label}
            </option>
          ))}
        </select>
      </label>
      <label style={{ display: "block", marginBottom: 12 }}>
        <span className="kea-label">Preset</span>
        <select
          className="kea-select"
          value={presetId}
          onChange={(e) => {
            const pid = e.target.value;
            setPresetId(pid);
            persist(mode, pid, customInstruction);
          }}
          style={{ minWidth: 220 }}
        >
          <option value="">None (use mode template)</option>
          {presets.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      </label>
      {mode === "ask_kea" && (
        <label style={{ display: "block", marginBottom: 12 }}>
          <span className="kea-label">Custom instruction</span>
          <textarea
            className="kea-input"
            value={customInstruction}
            onChange={(e) => {
              // Typing counts as interaction so a slow mount fetch can't
              // clobber text entered before it resolves (persist is on blur).
              userInteractedRef.current = true;
              setCustomInstruction(e.target.value);
            }}
            onBlur={(e) => persist(mode, presetId, e.target.value)}
            rows={3}
            style={{ maxWidth: 480, resize: "vertical" }}
            placeholder="Tell KEA what to do with the selection…"
          />
        </label>
      )}
      <label style={{ display: "block", marginBottom: 12 }}>
        <span className="kea-label">Prompt override (optional)</span>
        <textarea
          className="kea-input"
          value={promptOverride}
          onChange={(e) => setPromptOverrideText(e.target.value)}
          rows={3}
          style={{ maxWidth: 480, resize: "vertical" }}
          placeholder="Override the built-in prompt for the selected mode"
        />
      </label>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 16 }}>
        <button type="button" className="kea-btn" onClick={savePromptOverride} disabled={busy}>
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
            style={{ minWidth: 140, maxWidth: 200 }}
          />
          <input
            className="kea-input"
            value={newPresetInstruction}
            onChange={(e) => setNewPresetInstruction(e.target.value)}
            placeholder="Instruction"
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
      {status && (
        <p className="kea-muted" style={{ marginTop: 12 }}>
          {status}
        </p>
      )}
        </>
      )}
    </section>
  );
}
