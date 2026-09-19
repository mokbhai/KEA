import { useCallback, useEffect, useState } from "react";
import {
  captureScreenText,
  clearPaletteHistory,
  getOcrLanguages,
  getSetting,
  openPalette,
  setSetting,
} from "../api";
import { toMessage } from "../lib/format";
import HotkeyRow from "./HotkeyRow";
import { Row, RowGroup } from "./SettingsRow";
import Toggle from "./Toggle";

/** Mirrors `commands::PALETTE_VERIFY_SETTING`. */
const VERIFY_KEY = "palette.verify_selection";
/** Mirrors `kea_core::rewrite::STORE_HISTORY_SETTING`. */
const STORE_HISTORY_KEY = "palette.store_history";
/** Mirrors `commands::OCR_LANGUAGE_CORRECTION_SETTING`. */
const OCR_CORRECTION_KEY = "ocr.language_correction";
/** Mirrors `commands::OCR_LANGUAGES_SETTING`. */
const OCR_LANGUAGES_KEY = "ocr.languages";

/**
 * `set_setting` takes a string and JSON-encodes it, so every toggle on this
 * page is stored as the JSON string `"true"` / `"false"` and read back the
 * same way. The backend's `bool_setting` helper tolerates both encodings; this
 * is the frontend half of that contract. An absent row means the default.
 */
function readFlag(value: string | null, fallback: boolean): boolean {
  if (value === "true") return true;
  if (value === "false") return false;
  return fallback;
}

type Flags = {
  verify: boolean;
  storeHistory: boolean;
  ocrCorrection: boolean;
  ocrLanguages: string;
};

const DEFAULTS: Flags = {
  verify: true,
  storeHistory: true,
  ocrCorrection: true,
  ocrLanguages: "",
};

/**
 * The palette's and the screen-capture shortcut's settings.
 *
 * Both live on the Rewrite page because both are rewrite commands — same
 * feature, same `llm` slot binding, same presets. Giving them a page of their
 * own would imply a second AI configuration that does not exist.
 */
export default function PaletteSettings() {
  const [flags, setFlags] = useState<Flags>(DEFAULTS);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [languages, setLanguages] = useState<string[]>([]);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const [verify, storeHistory, ocrCorrection, ocrLanguages] = await Promise.all([
          getSetting(VERIFY_KEY),
          getSetting(STORE_HISTORY_KEY),
          getSetting(OCR_CORRECTION_KEY),
          getSetting(OCR_LANGUAGES_KEY),
        ]);
        if (!live) return;
        setFlags({
          verify: readFlag(verify, DEFAULTS.verify),
          storeHistory: readFlag(storeHistory, DEFAULTS.storeHistory),
          ocrCorrection: readFlag(ocrCorrection, DEFAULTS.ocrCorrection),
          ocrLanguages: ocrLanguages ?? "",
        });
      } catch (e) {
        if (live) setError(toMessage(e));
      }
      // Asked of Vision rather than shipped as a guessed list: which languages
      // an OS build recognises differs by macOS version. A failure here is
      // ordinary (non-macOS builds have a stub) and leaves the hint off.
      try {
        const supported = await getOcrLanguages();
        if (live) setLanguages(supported);
      } catch {
        /* no list to show */
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  /** Optimistic with rollback: the UI never keeps showing a refused value. */
  const save = useCallback(
    async (patch: Partial<Flags>, key: string, value: string) => {
      const previous = flags;
      setFlags({ ...previous, ...patch });
      setError(null);
      try {
        await setSetting(key, value);
      } catch (e) {
        setFlags(previous);
        setError(toMessage(e));
      }
    },
    [flags],
  );

  /**
   * The non-shortcut way in, for the case the shortcut cannot cover: another
   * app already owns the combo and the OS refused to register it. The hints
   * are blunt about what happens when the palette is opened from here, because
   * the frontmost app is KEA and there is nothing selected in it.
   */
  const run = async (action: () => Promise<void>) => {
    setError(null);
    setStatus(null);
    try {
      await action();
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const onClearHistory = async () => {
    setError(null);
    try {
      await clearPaletteHistory();
      setStatus("Instruction history cleared.");
    } catch (e) {
      setError(toMessage(e));
    }
  };

  return (
    <section style={{ marginBottom: 24 }}>
      <h2 style={{ margin: "0 0 12px" }}>Prompt palette</h2>
      <p className="kea-muted" style={{ margin: "0 0 12px" }}>
        A bar that takes one instruction against whatever you have selected, and
        puts the answer back where it came from. Return replaces the selection,
        ⌘Return inserts at the cursor, ⇧⌘Return copies.
      </p>
      <RowGroup aria-label="Prompt palette">
        <HotkeyRow
          feature="rewrite"
          command="prompt_palette"
          label="Open the palette"
          hint="Reads your selection first, then opens."
          checkRegistration
        />
        <HotkeyRow
          feature="rewrite"
          command="ocr_capture"
          label="Capture screen text"
          hint="Select a region, then ask about the text in it. Esc cancels and opens nothing."
          checkRegistration
        />
        <Row
          label="Check the selection before replacing it"
          hint="Costs about 150ms. Some apps drop the selection when the palette takes focus; without this check the answer would overwrite whatever is there instead."
        >
          <Toggle
            checked={flags.verify}
            onChange={(next) =>
              void save({ verify: next }, VERIFY_KEY, String(next))
            }
            label="Check the selection before replacing it"
          />
        </Row>
        <Row
          label="Remember instructions"
          hint="Press the up arrow in the palette to reuse one. Turning off history for conversations turns this off too."
        >
          <Toggle
            checked={flags.storeHistory}
            onChange={(next) =>
              void save({ storeHistory: next }, STORE_HISTORY_KEY, String(next))
            }
            label="Remember palette instructions"
          />
        </Row>
        <Row label="Instruction history" hint="Forgets every saved instruction.">
          <button type="button" className="kea-btn" onClick={() => void onClearHistory()}>
            Clear
          </button>
        </Row>
        <Row
          label="Correct spelling in captured text"
          hint="Right for prose, wrong for code, identifiers and serial numbers — turn it off before capturing a terminal."
        >
          <Toggle
            checked={flags.ocrCorrection}
            onChange={(next) =>
              void save({ ocrCorrection: next }, OCR_CORRECTION_KEY, String(next))
            }
            label="Correct spelling in captured text"
          />
        </Row>
        <Row
          label="Capture languages"
          hint={
            languages.length > 0
              ? `Comma-separated, most preferred first. Leave empty to let macOS choose. This Mac supports: ${languages.join(", ")}.`
              : "Comma-separated BCP-47 tags, most preferred first. Leave empty to let macOS choose."
          }
        >
          <input
            className="kea-input"
            type="text"
            aria-label="Capture languages"
            placeholder="en-US, ja"
            value={flags.ocrLanguages}
            onChange={(e) => setFlags({ ...flags, ocrLanguages: e.target.value })}
            // Persisted on blur, not per keystroke: a settings write per
            // character is a write per character.
            onBlur={(e) =>
              void save({ ocrLanguages: e.target.value }, OCR_LANGUAGES_KEY, e.target.value)
            }
          />
        </Row>
      </RowGroup>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 12 }}>
        <button type="button" className="kea-btn" onClick={() => void run(openPalette)}>
          Open the palette
        </button>
        <button
          type="button"
          className="kea-btn"
          onClick={() => void run(captureScreenText)}
        >
          Capture screen text
        </button>
      </div>
      <p className="kea-muted" style={{ margin: "8px 0 0", fontSize: "0.8125rem" }}>
        Opened from here the palette has no selection to work on — KEA is the
        app in front — so it opens as a plain question. The shortcut is the real
        entry point.
      </p>

      {status && (
        <p className="kea-muted" style={{ margin: "8px 0 0" }}>
          {status}
        </p>
      )}
      {error && (
        <p className="kea-row__hint--danger" style={{ margin: "8px 0 0" }} role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
