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
import { toMessage } from "../lib/format";

/** What a rewrite run is parameterised by: the style, the preset, the free text. */
export type RewriteSettings = {
  mode: RewriteMode;
  preset_id: string | null;
  custom_instruction: string;
};

/**
 * The saved rewrite behaviour, loaded once and written back on every change.
 *
 * The state lives here because two consumers need it: the form that edits it
 * and the page that runs a rewrite with it. It used to live inside the form,
 * which meant the page kept a second copy fed by an `onChange` effect — the
 * classic two-sources-of-truth arrangement, with the form pushing on every
 * render and the page one render behind.
 */
export type RewriteSettingsController = {
  settings: RewriteSettings;
  presets: RewritePreset[];
  /** The prompt override for the currently selected style, "" when none. */
  promptOverride: string;
  /** True until the mount fetch settles; the form shows a spinner instead. */
  loading: boolean;
  /** A write is in flight: the form disables its buttons. */
  busy: boolean;
  status: string | null;
  chooseMode: (mode: RewriteMode) => void;
  choosePreset: (presetId: string) => void;
  /** Typing: local only, so the write lands on blur rather than per keystroke. */
  editCustomInstruction: (text: string) => void;
  commitCustomInstruction: (text: string) => void;
  editPromptOverride: (text: string) => void;
  savePromptOverride: () => Promise<void>;
  /** Resolves true once the preset is stored, so the form can clear its inputs. */
  addPreset: (name: string, instruction: string) => Promise<boolean>;
  removePreset: (id: string) => Promise<void>;
};

const DEFAULT_MODE: RewriteMode = "improve";

export function useRewriteSettings(): RewriteSettingsController {
  const [mode, setMode] = useState<RewriteMode>(DEFAULT_MODE);
  const [presetId, setPresetId] = useState("");
  const [customInstruction, setCustomInstruction] = useState("");
  const [promptOverride, setPromptOverrideText] = useState("");
  const [presets, setPresets] = useState<RewritePreset[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  /** Set by the first edit: the mount fetch must not clobber what it replaced. */
  const editedRef = useRef(false);

  const loadPresets = () =>
    listPresets()
      .then(setPresets)
      .catch((e) => setStatus(toMessage(e)));

  const persist = useCallback((m: RewriteMode, pid: string, ci: string) => {
    editedRef.current = true;
    setBusy(true);
    setStatus(null);
    Promise.all([
      setSetting("rewrite.active_mode", m),
      setSetting("rewrite.active_preset_id", pid),
      setSetting("rewrite.custom_instruction", ci),
    ])
      .then(() => setStatus("Rewrite settings saved."))
      .catch((e) => setStatus(toMessage(e)))
      .finally(() => setBusy(false));
  }, []);

  useEffect(() => {
    Promise.all([
      getSetting("rewrite.active_mode"),
      getSetting("rewrite.active_preset_id"),
      getSetting("rewrite.custom_instruction"),
      loadPresets(),
    ])
      .then(([activeMode, activePreset, customInst]) => {
        // An edit made while this was in flight already went to the store;
        // applying what the store held before it would undo the user.
        if (editedRef.current) return;
        if (activeMode && REWRITE_MODES.some((m) => m.value === activeMode)) {
          setMode(activeMode as RewriteMode);
        }
        setPresetId(activePreset ?? "");
        setCustomInstruction(customInst ?? "");
      })
      .catch((e) => setStatus(toMessage(e)))
      .finally(() => setLoading(false));
    // Runs once: this is the mount fetch.
  }, []);

  useEffect(() => {
    getPromptOverride(mode)
      .then((value) => setPromptOverrideText(value ?? ""))
      .catch((e) => setStatus(toMessage(e)));
  }, [mode]);

  const chooseMode = (m: RewriteMode) => {
    setMode(m);
    persist(m, presetId, customInstruction);
  };

  const choosePreset = (pid: string) => {
    setPresetId(pid);
    persist(mode, pid, customInstruction);
  };

  const editCustomInstruction = (text: string) => {
    // Typing counts as interaction so a slow mount fetch can't clobber text
    // entered before it resolves (the write is on blur).
    editedRef.current = true;
    setCustomInstruction(text);
  };

  const savePromptOverride = async () => {
    setBusy(true);
    setStatus(null);
    try {
      await setPromptOverride(mode, promptOverride);
      setStatus("Prompt override saved.");
    } catch (e) {
      setStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const addPreset = async (name: string, instruction: string) => {
    if (!name.trim() || !instruction.trim()) return false;
    setBusy(true);
    setStatus(null);
    try {
      const id = `preset-${Date.now()}`;
      await upsertPreset({ id, name: name.trim(), instruction: instruction.trim() });
      await loadPresets();
      setPresetId(id);
      persist(mode, id, customInstruction);
      setStatus("Preset added.");
      return true;
    } catch (e) {
      setStatus(toMessage(e));
      return false;
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
      setStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return {
    settings: {
      mode,
      preset_id: presetId || null,
      custom_instruction: customInstruction,
    },
    presets,
    promptOverride,
    loading,
    busy,
    status,
    chooseMode,
    choosePreset,
    editCustomInstruction,
    commitCustomInstruction: (text) => persist(mode, presetId, text),
    editPromptOverride: setPromptOverrideText,
    savePromptOverride,
    addPreset,
    removePreset,
  };
}
