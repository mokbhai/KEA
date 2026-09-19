import { useCallback, useEffect, useRef, useState } from "react";
import {
  MODE_PARAMETER,
  REWRITE_MODES,
  deletePreset,
  getPromptOverride,
  getSetting,
  listPresets,
  setPromptOverride,
  setSetting,
  systemTranslationTarget,
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
  /** Translate's target, as a BCP-47 tag. */
  translate_target: string;
};

/**
 * The one extra value the chosen mode's prompt needs, or null when it needs
 * none — the shape both rewrite entry points take as their last argument.
 */
export function modeParameter(settings: RewriteSettings): string | null {
  switch (MODE_PARAMETER[settings.mode]) {
    case "instruction":
      return settings.custom_instruction || null;
    case "target_language":
      return settings.translate_target || null;
    default:
      return null;
  }
}

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
  /** {@link modeParameter} of the current settings, for whoever runs them. */
  parameter: string | null;
  presets: RewritePreset[];
  /** Tags with a translate shortcut of their own, in the order shown. */
  translateTargets: string[];
  /** The prompt override for the currently selected style, "" when none. */
  promptOverride: string;
  /** True until the mount fetch settles; the form shows a spinner instead. */
  loading: boolean;
  /** A write is in flight: the form disables its buttons. */
  busy: boolean;
  status: string | null;
  chooseMode: (mode: RewriteMode) => void;
  choosePreset: (presetId: string) => void;
  chooseTranslateTarget: (tag: string) => void;
  /** No-op when `tag` is already listed, so the list stays a set. */
  addTranslateTarget: (tag: string) => Promise<void>;
  removeTranslateTarget: (tag: string) => Promise<void>;
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
const TARGETS_KEY = "rewrite.translate.targets";

/** The stored list, tolerant of a key never written or written as junk. */
function parseTargets(raw: string | null): string[] {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((t): t is string => typeof t === "string");
  } catch {
    return [];
  }
}

export function useRewriteSettings(): RewriteSettingsController {
  const [mode, setMode] = useState<RewriteMode>(DEFAULT_MODE);
  const [presetId, setPresetId] = useState("");
  const [customInstruction, setCustomInstruction] = useState("");
  // Seeded from the system language: translating into your own is what an
  // unconfigured shortcut is for, and it beats showing a target the user never
  // chose as if they had.
  const [translateTarget, setTranslateTarget] = useState(systemTranslationTarget);
  const [translateTargets, setTranslateTargets] = useState<string[]>([]);
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

  const persist = useCallback((next: RewriteSettings) => {
    editedRef.current = true;
    setBusy(true);
    setStatus(null);
    Promise.all([
      setSetting("rewrite.active_mode", next.mode),
      setSetting("rewrite.active_preset_id", next.preset_id ?? ""),
      setSetting("rewrite.custom_instruction", next.custom_instruction),
      setSetting("rewrite.translate.target", next.translate_target),
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
      getSetting("rewrite.translate.target"),
      getSetting(TARGETS_KEY),
      loadPresets(),
    ])
      .then(([activeMode, activePreset, customInst, target, targets]) => {
        // An edit made while this was in flight already went to the store;
        // applying what the store held before it would undo the user.
        if (editedRef.current) return;
        if (activeMode && REWRITE_MODES.some((m) => m.value === activeMode)) {
          setMode(activeMode as RewriteMode);
        }
        setPresetId(activePreset ?? "");
        setCustomInstruction(customInst ?? "");
        // Absent or blank keeps the system-language seed rather than blanking
        // the picker.
        if (target) setTranslateTarget(target);
        setTranslateTargets(parseTargets(targets));
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

  const settings: RewriteSettings = {
    mode,
    preset_id: presetId || null,
    custom_instruction: customInstruction,
    translate_target: translateTarget,
  };

  const chooseMode = (m: RewriteMode) => {
    setMode(m);
    persist({ ...settings, mode: m });
  };

  const choosePreset = (pid: string) => {
    setPresetId(pid);
    persist({ ...settings, preset_id: pid || null });
  };

  const chooseTranslateTarget = (tag: string) => {
    setTranslateTarget(tag);
    persist({ ...settings, translate_target: tag });
  };

  /** One write for the whole list: the key stores it as a JSON array. */
  const saveTargets = async (next: string[], done: string) => {
    // Same guard the settings writes use: the section renders before the mount
    // fetch settles, so a language added meanwhile must survive it.
    editedRef.current = true;
    setBusy(true);
    setStatus(null);
    const previous = translateTargets;
    setTranslateTargets(next);
    try {
      await setSetting(TARGETS_KEY, JSON.stringify(next));
      setStatus(done);
    } catch (e) {
      setTranslateTargets(previous);
      setStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const addTranslateTarget = async (tag: string) => {
    if (!tag || translateTargets.includes(tag)) return;
    await saveTargets([...translateTargets, tag], "Language added.");
  };

  const removeTranslateTarget = (tag: string) =>
    saveTargets(
      translateTargets.filter((t) => t !== tag),
      "Language removed.",
    );

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
      persist({ ...settings, preset_id: id });
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
        persist({ ...settings, preset_id: null });
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
    settings,
    parameter: modeParameter(settings),
    presets,
    translateTargets,
    promptOverride,
    loading,
    busy,
    status,
    chooseMode,
    choosePreset,
    chooseTranslateTarget,
    addTranslateTarget,
    removeTranslateTarget,
    editCustomInstruction,
    commitCustomInstruction: (text) => persist({ ...settings, custom_instruction: text }),
    editPromptOverride: setPromptOverrideText,
    savePromptOverride,
    addPreset,
    removePreset,
  };
}
