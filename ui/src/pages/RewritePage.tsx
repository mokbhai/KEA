import { useState } from "react";
import {
  TRANSLATION_TARGETS,
  previewRewrite,
  translateCommand,
  translationTargetLabel,
  triggerRewrite,
  undoLastRewrite,
} from "../api";
import FeatureAiCard from "../components/FeatureAiCard";
import FeatureBanner from "../components/FeatureBanner";
import HotkeyRow from "../components/HotkeyRow";
import PaletteSettings from "../components/PaletteSettings";
import SettingsForm from "../components/SettingsForm";
import { RowGroup } from "../components/SettingsRow";
import Spinner from "../components/Spinner";
import { useFeatureAi } from "../hooks/useFeatureAi";
import { useRewriteSettings } from "../hooks/useRewriteSettings";
import type { SlotSpec } from "../lib/featureSlot";
import { toMessage } from "../lib/format";
import type { Navigate } from "../lib/nav";

const SAMPLE = "i think we should probaly ship this on friday, lmk what u think";

const SLOTS: SlotSpec[] = [
  { feature: "rewrite", slot: "llm", capability: "llm", label: "Writing & rewriting" },
];

type Props = {
  onRunSetup?: () => void;
  onNavigate?: Navigate;
};

export default function RewritePage({ onRunSetup, onNavigate }: Props) {
  const ai = useFeatureAi(SLOTS);
  const rewrite = useRewriteSettings();
  const { settings, translateTargets } = rewrite;
  const [sample, setSample] = useState(SAMPLE);
  const [newTarget, setNewTarget] = useState("");
  const [result, setResult] = useState<string | null>(null);
  const [runStatus, setRunStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const runSample = async () => {
    setBusy(true);
    setRunStatus(null);
    setResult(null);
    try {
      const text = await previewRewrite(
        sample,
        settings.mode,
        settings.preset_id,
        rewrite.parameter,
      );
      setResult(text);
    } catch (e) {
      setRunStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  /**
   * The undo, from the window rather than the shortcut.
   *
   * It brings the app the rewrite happened in back to the front before
   * writing, so pressing this from here is the same act the key is — and every
   * refusal it can return names its reason, which is why the message is shown
   * verbatim.
   */
  const undoSelection = async () => {
    setBusy(true);
    setRunStatus(null);
    setResult(null);
    try {
      await undoLastRewrite();
      setRunStatus("Put your text back.");
    } catch (e) {
      setRunStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  // The real shortcut path: rewrites whatever is selected in the app you were
  // last in, and replaces it there.
  const runSelection = async () => {
    setBusy(true);
    setRunStatus(null);
    setResult(null);
    try {
      const text = await triggerRewrite(
        settings.mode,
        settings.preset_id,
        rewrite.parameter,
      );
      setRunStatus(
        text ? "Rewritten and replaced in the app you were last in." : "Rewrite completed.",
      );
    } catch (e) {
      setRunStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>Rewrite</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
          Select text anywhere on your Mac and press the shortcut to rewrite it in
          place.
        </p>
      </header>

      <FeatureBanner ai={ai} onNavigate={onNavigate} />

      {onRunSetup && ai.statuses?.some((s) => s.blocked) && (
        <div style={{ marginBottom: 16 }}>
          <button type="button" className="kea-btn" onClick={onRunSetup}>
            Run setup again
          </button>
        </div>
      )}

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Behavior</h2>
        <SettingsForm
          rewrite={rewrite}
          leadingRows={
            <>
              <HotkeyRow
                feature="rewrite"
                command="rewrite_selection"
                label="Shortcut"
                hint="Rewrites the text you have selected."
                checkRegistration
              />
              <HotkeyRow
                feature="rewrite"
                command="undo_rewrite"
                label="Put my words back"
                hint="Restores the text the last rewrite replaced, for about two minutes afterwards. It refuses rather than guessing if what KEA wrote is no longer there. Try your app's own Undo first — this is for when that does not reach it."
                checkRegistration
              />
            </>
          }
        />
      </section>

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Languages and shortcuts</h2>
        <div className="kea-card">
          <p className="kea-muted" style={{ margin: "0 0 12px" }}>
            Each language here gets a shortcut of its own that translates the
            selection straight into it, whatever style is chosen above. Nothing is
            bound until you record a combo.
          </p>
          {translateTargets.length > 0 && (
            <RowGroup aria-label="Translate shortcuts">
              {translateTargets.map((tag) => (
                <HotkeyRow
                  key={tag}
                  feature="rewrite"
                  command={translateCommand(tag)}
                  label={translationTargetLabel(tag)}
                  hint={`Translates the selection into ${translationTargetLabel(tag)}.`}
                  checkRegistration
                />
              ))}
            </RowGroup>
          )}
          <div
            style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 12 }}
          >
            <select
              className="kea-select"
              aria-label="Language to add"
              value={newTarget}
              onChange={(e) => setNewTarget(e.target.value)}
            >
              <option value="">Add a language…</option>
              {TRANSLATION_TARGETS.filter((t) => !translateTargets.includes(t.tag)).map(
                (t) => (
                  <option key={t.tag} value={t.tag}>
                    {t.label}
                  </option>
                ),
              )}
            </select>
            <button
              type="button"
              className="kea-btn"
              disabled={!newTarget || rewrite.busy}
              onClick={() => {
                void rewrite.addTranslateTarget(newTarget);
                setNewTarget("");
              }}
            >
              Add
            </button>
          </div>
          {translateTargets.length > 0 && (
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 12 }}>
              {translateTargets.map((tag) => (
                <button
                  key={tag}
                  type="button"
                  className="kea-btn"
                  disabled={rewrite.busy}
                  onClick={() => void rewrite.removeTranslateTarget(tag)}
                >
                  Remove {translationTargetLabel(tag)}
                </button>
              ))}
            </div>
          )}
        </div>
      </section>

      <PaletteSettings />

      <FeatureAiCard ai={ai} featureLabel="Rewrite" />

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Try it</h2>
        <div className="kea-card">
          <label style={{ display: "block", marginBottom: 12 }}>
            <span className="kea-label">Sample text</span>
            <textarea
              className="kea-input"
              aria-label="Sample text"
              value={sample}
              onChange={(e) => setSample(e.target.value)}
              rows={3}
              style={{ width: "100%", maxWidth: 520, resize: "vertical" }}
            />
          </label>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              type="button"
              className="kea-btn kea-btn--primary"
              onClick={() => void runSample()}
              disabled={busy || !sample.trim()}
            >
              {busy ? (
                <>
                  <Spinner size={14} /> Rewriting…
                </>
              ) : (
                "Rewrite this"
              )}
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void runSelection()}
              disabled={busy}
            >
              Rewrite my selection
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void undoSelection()}
              disabled={busy}
            >
              Put my words back
            </button>
          </div>
          <p className="kea-muted" style={{ margin: "8px 0 0", fontSize: "0.8125rem" }}>
            "Rewrite this" shows the result here and pastes nothing anywhere.
            "Rewrite my selection" runs the real shortcut and replaces the text
            you have selected in another app.
          </p>
          {result !== null && (
            <div style={{ marginTop: 12 }}>
              <span className="kea-label">Result</span>
              <p
                style={{
                  margin: "4px 0 0",
                  padding: 12,
                  background: "var(--surface-2)",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  whiteSpace: "pre-wrap",
                }}
              >
                {result}
              </p>
            </div>
          )}
          {runStatus && (
            <p className="kea-muted" style={{ marginTop: 12, marginBottom: 0 }}>
              {runStatus}
            </p>
          )}
        </div>
      </section>
    </div>
  );
}
