import { useEffect, useState } from "react";
import { getEffectiveHotkey } from "../../api";
import Banner from "../Banner";
import HotkeyRow, { formatAccelerator } from "../HotkeyRow";
import { RowGroup } from "../SettingsRow";

const FEATURE_HOTKEYS = [
  {
    feature: "rewrite",
    command: "rewrite_selection",
    label: "Rewrite",
    hint: "Rewrites the text you have selected.",
  },
  {
    feature: "dictation",
    command: "push_to_talk",
    label: "Dictation",
    hint: "Hold to talk, release to type what you said.",
  },
  {
    feature: "meetings",
    command: "toggle_meeting",
    label: "Meetings",
    hint: "Starts or stops meeting notes.",
  },
  {
    feature: "tts",
    command: "read_selection",
    label: "Read aloud",
    hint: "Reads the selected text out loud.",
  },
] as const;

export default function HotkeysStep() {
  // The raw Rewrite accelerator; the row below updates it after re-recording
  // so the "try it now" line never shows a stale combo.
  const [rewriteAccel, setRewriteAccel] = useState<string | null>(null);

  useEffect(() => {
    getEffectiveHotkey("rewrite", "rewrite_selection")
      .then((hk) => setRewriteAccel(hk ? hk.accelerator : null))
      .catch(() => setRewriteAccel(null));
  }, []);

  return (
    <div>
      <p className="kea-muted" style={{ marginTop: 0, marginBottom: 16 }}>
        These shortcuts work anywhere on your Mac. Change one by re-recording it.
      </p>
      <RowGroup aria-label="Hotkeys">
        {FEATURE_HOTKEYS.map((f) => (
          <HotkeyRow
            key={`${f.feature}/${f.command}`}
            feature={f.feature}
            command={f.command}
            label={f.label}
            hint={f.hint}
            onSaved={f.feature === "rewrite" ? setRewriteAccel : undefined}
          />
        ))}
      </RowGroup>
      <div style={{ marginTop: 16 }}>
        <Banner variant="ok">
          You're ready! Try it now: select some text in any app and press{" "}
          <strong>{rewriteAccel ? formatAccelerator(rewriteAccel) : "the Rewrite shortcut"}</strong>{" "}
          to rewrite it.
        </Banner>
      </div>
    </div>
  );
}
