import { useCallback, useEffect, useRef, useState } from "react";
import {
  MODE_PARAMETER,
  clearHotkey,
  deleteCustomStyle,
  getPromptOverride,
  getSetting,
  isBuiltinStyle,
  listCustomStyles,
  setPromptOverride,
  setSetting,
  systemTranslationTarget,
  translateCommand,
  upsertCustomStyle,
  type CustomStyle,
  type StyleValue,
} from "../api";
import { toMessage } from "../lib/format";

/** What a rewrite run is parameterised by: the style and the free text. */
export type RewriteSettings = {
  /** A built-in tag or a custom style id — one value, see {@link StyleValue}. */
  style: StyleValue;
  custom_instruction: string;
  /** Translate's target, as a BCP-47 tag. */
  translate_target: string;
};

/**
 * The one extra value the chosen style's prompt needs, or null when it needs
 * none — the shape both rewrite entry points take as their last argument. A
 * custom style carries its whole prompt, so it never needs one.
 */
export function modeParameter(settings: RewriteSettings): string | null {
  if (!isBuiltinStyle(settings.style)) return null;
  switch (MODE_PARAMETER[settings.style]) {
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
  /** The user's own styles, in the order the picker lists them. */
  styles: CustomStyle[];
  /** Tags with a translate shortcut of their own, in the order shown. */
  translateTargets: string[];
  /** The prompt override for the selected built-in style, "" when none. */
  promptOverride: string;
  /** True until the mount fetch settles; the form shows a spinner instead. */
  loading: boolean;
  /** A write is in flight: the form disables its buttons. */
  busy: boolean;
  status: string | null;
  chooseStyle: (style: StyleValue) => void;
  chooseTranslateTarget: (tag: string) => void;
  /** No-op when `tag` is already listed, so the list stays a set. */
  addTranslateTarget: (tag: string) => Promise<void>;
  removeTranslateTarget: (tag: string) => Promise<void>;
  /** Typing: local only, so the write lands on blur rather than per keystroke. */
  editCustomInstruction: (text: string) => void;
  commitCustomInstruction: (text: string) => void;
  editPromptOverride: (text: string) => void;
  savePromptOverride: () => Promise<void>;
  /** Resolves true once the style is stored, so the form can clear its inputs. */
  addStyle: (name: string, instruction: string) => Promise<boolean>;
  /** Resolves true once the edit is stored, so the form can close its editor. */
  saveStyle: (style: CustomStyle) => Promise<boolean>;
  /**
   * Deletes a style. The backend falls the active style back to Improve when
   * it was the one deleted, and this mirrors that so the picker agrees.
   */
  removeStyle: (id: string) => Promise<void>;
};

const DEFAULT_STYLE: StyleValue = "improve";
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
  const [style, setStyle] = useState<StyleValue>(DEFAULT_STYLE);
  const [customInstruction, setCustomInstruction] = useState("");
  // Seeded from the system language: translating into your own is what an
  // unconfigured shortcut is for, and it beats showing a target the user never
  // chose as if they had.
  const [translateTarget, setTranslateTarget] = useState(systemTranslationTarget);
  const [translateTargets, setTranslateTargets] = useState<string[]>([]);
  const [promptOverride, setPromptOverrideText] = useState("");
  const [styles, setStyles] = useState<CustomStyle[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  /** Set by the first edit: the mount fetch must not clobber what it replaced. */
  const editedRef = useRef(false);

  const loadStyles = () =>
    listCustomStyles()
      .then((list) => {
        setStyles(list);
        return list;
      })
      .catch((e) => {
        setStatus(toMessage(e));
        return [] as CustomStyle[];
      });

  const persist = useCallback((next: RewriteSettings) => {
    editedRef.current = true;
    setBusy(true);
    setStatus(null);
    Promise.all([
      setSetting("rewrite.active_mode", next.style),
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
      getSetting("rewrite.custom_instruction"),
      getSetting("rewrite.translate.target"),
      getSetting(TARGETS_KEY),
      loadStyles(),
    ])
      .then(([activeStyle, customInst, target, targets, list]) => {
        // An edit made while this was in flight already went to the store;
        // applying what the store held before it would undo the user.
        if (editedRef.current) return;
        // One stored value names either kind. A custom id that no longer
        // exists reads as Improve, the same fallback the backend applies.
        if (activeStyle && (isBuiltinStyle(activeStyle) || list.some((s) => s.id === activeStyle))) {
          setStyle(activeStyle);
        }
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
    // Only a built-in style has a template to override.
    if (!isBuiltinStyle(style)) {
      setPromptOverrideText("");
      return;
    }
    getPromptOverride(style)
      .then((value) => setPromptOverrideText(value ?? ""))
      .catch((e) => setStatus(toMessage(e)));
  }, [style]);

  const settings: RewriteSettings = {
    style,
    custom_instruction: customInstruction,
    translate_target: translateTarget,
  };

  const chooseStyle = (value: StyleValue) => {
    setStyle(value);
    persist({ ...settings, style: value });
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

  /**
   * Drop a language, and its shortcut with it.
   *
   * The unbind comes first and is awaited: a combo left registered for a
   * language that no longer appears anywhere would keep stealing that key from
   * every other app, with no screen offering a way to get it back.
   */
  const removeTranslateTarget = async (tag: string) => {
    try {
      await clearHotkey("rewrite", translateCommand(tag));
    } catch (e) {
      setStatus(toMessage(e));
      return;
    }
    await saveTargets(
      translateTargets.filter((t) => t !== tag),
      "Language removed.",
    );
  };

  const editCustomInstruction = (text: string) => {
    // Typing counts as interaction so a slow mount fetch can't clobber text
    // entered before it resolves (the write is on blur).
    editedRef.current = true;
    setCustomInstruction(text);
  };

  const savePromptOverride = async () => {
    if (!isBuiltinStyle(style)) return;
    setBusy(true);
    setStatus(null);
    try {
      await setPromptOverride(style, promptOverride);
      setStatus("Prompt override saved.");
    } catch (e) {
      setStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const addStyle = async (name: string, instruction: string) => {
    if (!name.trim() || !instruction.trim()) return false;
    editedRef.current = true;
    setBusy(true);
    setStatus(null);
    try {
      await upsertCustomStyle({
        id: `style-${Date.now()}`,
        name: name.trim(),
        instruction: instruction.trim(),
      });
      await loadStyles();
      setStatus("Style added. Choose it from the Style menu to use it.");
      return true;
    } catch (e) {
      setStatus(toMessage(e));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const saveStyle = async (edited: CustomStyle) => {
    if (!edited.name.trim() || !edited.instruction.trim()) return false;
    editedRef.current = true;
    setBusy(true);
    setStatus(null);
    try {
      await upsertCustomStyle({
        ...edited,
        name: edited.name.trim(),
        instruction: edited.instruction.trim(),
      });
      await loadStyles();
      setStatus("Style saved.");
      return true;
    } catch (e) {
      setStatus(toMessage(e));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const removeStyle = async (id: string) => {
    editedRef.current = true;
    setBusy(true);
    setStatus(null);
    try {
      await deleteCustomStyle(id);
      // The backend has already reset the stored selection; mirror it rather
      // than writing it again, so the picker cannot disagree with the store.
      if (style === id) setStyle(DEFAULT_STYLE);
      await loadStyles();
      setStatus("Style deleted.");
    } catch (e) {
      setStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return {
    settings,
    parameter: modeParameter(settings),
    styles,
    translateTargets,
    promptOverride,
    loading,
    busy,
    status,
    chooseStyle,
    chooseTranslateTarget,
    addTranslateTarget,
    removeTranslateTarget,
    editCustomInstruction,
    commitCustomInstruction: (text) => persist({ ...settings, custom_instruction: text }),
    editPromptOverride: setPromptOverrideText,
    savePromptOverride,
    addStyle,
    saveStyle,
    removeStyle,
  };
}
