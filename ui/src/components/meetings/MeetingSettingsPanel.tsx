import { useEffect, useRef, type ReactNode, type RefObject } from "react";
import type {
  MeetingSettings,
  NotionStatus,
  PermStatus,
  SystemAudioCapability,
} from "../../api";
import FeatureAiCard from "../FeatureAiCard";
import HotkeyRow from "../HotkeyRow";
import NotionSettings from "../NotionSettings";
import { Row, RowGroup } from "../SettingsRow";
import Toggle from "../Toggle";
import type { FeatureAi } from "../../hooks/useFeatureAi";
import type { OptimisticSetting } from "../../hooks/useOptimisticSetting";

const MEETINGS_FEATURE = "meetings";
const MEETINGS_COMMAND = "toggle_meeting";

const capabilityLabels: Record<SystemAudioCapability, string> = {
  unavailable: "Mic only (system audio unavailable)",
  mic_only: "Mic only",
  loopback_device: "Mic + system (loopback device)",
  screen_capture_kit: "Mic + system (ScreenCaptureKit)",
};

/**
 * The cost note that sits next to the interim-notes toggle.
 *
 * Spelled out rather than left as "uses AI": this spends the user's tokens on
 * a schedule they did not press a button for, and somebody who turns it on and
 * leaves a four-hour meeting running should not be surprised by the bill.
 */
function interimHint(settings: MeetingSettings): string {
  const everyMinutes = settings.interim_every_minutes;
  const everySegments = settings.interim_every_segments;
  return (
    `Writes notes every ${everySegments} segments or ${everyMinutes} minutes, ` +
    `whichever comes first — about ${Math.round(60 / Math.max(1, everyMinutes))} AI ` +
    "calls an hour, billed to your notes provider. Off by default."
  );
}

/**
 * The hint beside the voice-separation toggle.
 *
 * Says what the feature does with the audio, because turning it on means the
 * user's microphone and system audio are analysed for voices, and that is the
 * kind of thing somebody wants to read before they flip it.
 */
const VOICE_HINT =
  "Tells remote participants apart by voice, labelled Person 1, Person 2 and so on. " +
  "Runs on this Mac and nothing about voices is saved. Needs two speaker models.";

const TITLE_ID = "meeting-settings-title";

const FOCUSABLE =
  "button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), " +
  "textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";

/**
 * Keeps Tab inside the panel while it is open.
 *
 * `aria-modal` tells assistive tech the page behind is inert, but a keyboard
 * user's Tab would still walk off into it, and then the first Tab back lands on
 * a control the dialog is supposed to hide. Wrapping at both ends fixes that.
 */
function trapTab(e: KeyboardEvent, panel: HTMLElement | null) {
  if (!panel) return;
  const items = Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => el.closest("[hidden]") === null,
  );
  if (items.length === 0) return;
  const first = items[0];
  const last = items[items.length - 1];
  const active = document.activeElement;
  const outside = !panel.contains(active);
  if (e.shiftKey && (active === first || outside)) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && (active === last || outside)) {
    e.preventDefault();
    first.focus();
  }
}

type Props = {
  open: boolean;
  /** Closes the panel. The page owns the ⚙ button, so it also returns focus there. */
  onClose: () => void;
  /**
   * The ⚙ button. A press on it is not an outside click, because the button's
   * own toggle closes the panel; without this the press would close and reopen it.
   */
  anchorRef: RefObject<HTMLButtonElement>;
  settings: MeetingSettings;
  meetingSettings: OptimisticSetting<MeetingSettings>;
  saveSettings: (patch: Partial<MeetingSettings>, key: string) => Promise<void>;
  /**
   * A local edit to the segment length, with no write: the field persists on
   * blur through `saveSettings`, so an unsaved value survives closing the panel.
   */
  onSegmentDurationChange: (seconds: number) => void;
  capability: SystemAudioCapability;
  calendarPerm: PermStatus;
  requestCalendar: () => void | Promise<void>;
  actionBusy: boolean;
  voiceEnablePending: boolean;
  voiceModelsReady: boolean;
  voiceDownloading: boolean;
  voiceStatus: string | null;
  toggleVoiceSeparation: (next: boolean) => void | Promise<void>;
  downloadVoiceModels: () => void | Promise<void>;
  ai: FeatureAi;
  onNotionStatusChange: (status: NotionStatus) => void;
  /**
   * Extra controls the page places under the behaviour settings: the 10-second
   * test capture, which is a check of the setup rather than a setting.
   */
  checkSection?: ReactNode;
};

/**
 * Every meeting setting, in a panel that slides over the page from the right.
 *
 * The panel stays mounted while closed and is only hidden. That is deliberate:
 * Notion setup and the AI card keep their own unsaved drafts, and unmounting
 * on close would throw those away. Hiding also keeps the page's state hooks
 * where they are, so nothing the panel shows is copied or re-fetched on open.
 */
export default function MeetingSettingsPanel({
  open,
  onClose,
  anchorRef,
  settings,
  meetingSettings,
  saveSettings,
  onSegmentDurationChange,
  capability,
  calendarPerm,
  requestCalendar,
  actionBusy,
  voiceEnablePending,
  voiceModelsReady,
  voiceDownloading,
  voiceStatus,
  toggleVoiceSeparation,
  downloadVoiceModels,
  ai,
  onNotionStatusChange,
  checkSection,
}: Props) {
  const panelRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);

  // Read through a ref so the listeners below are attached once per opening,
  // not re-attached on every render of the page.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  // Moves focus into the panel on open. Keyed on `open` alone: re-running it on
  // every render would pull focus back to Close while the user is typing.
  useEffect(() => {
    if (open) closeRef.current?.focus();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onCloseRef.current();
        return;
      }
      if (e.key === "Tab") trapTab(e, panelRef.current);
    };
    // Mousedown, not click: a press that starts outside and ends inside a
    // control in the panel is still an outside press, and this matches what a
    // user expects a scrim to do.
    const onPress = (e: MouseEvent) => {
      const target = e.target as Node;
      if (panelRef.current?.contains(target)) return;
      if (anchorRef.current?.contains(target)) return;
      onCloseRef.current();
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onPress);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onPress);
    };
  }, [open, anchorRef]);

  return (
    <>
      <div className="kea-slideover-scrim" aria-hidden="true" hidden={!open} />
      <div
        ref={panelRef}
        className="kea-slideover"
        role="dialog"
        aria-modal="true"
        aria-labelledby={TITLE_ID}
        hidden={!open}
      >
        <div className="kea-slideover__header">
          <h2 id={TITLE_ID} style={{ margin: 0 }}>
            Meeting settings
          </h2>
          <button ref={closeRef} type="button" className="kea-btn" onClick={onClose}>
            Close
          </button>
        </div>

        <section style={{ marginBottom: 24 }}>
          <h2 style={{ margin: "0 0 12px" }}>Behavior</h2>
          <RowGroup aria-label="Meeting behavior">
            <HotkeyRow
              feature={MEETINGS_FEATURE}
              command={MEETINGS_COMMAND}
              label="Shortcut"
              hint="Starts or stops meeting notes."
              checkRegistration
            />
            <Row
              label="Record system audio too"
              hint={`Right now: ${capabilityLabels[capability]}.`}
            >
              {meetingSettings.savedKey === "prefer_system_audio" && (
                <span className="kea-saved">Saved ✓</span>
              )}
              <Toggle
                label="Record system audio too"
                checked={settings.prefer_system_audio}
                disabled={meetingSettings.busy}
                onChange={(next) =>
                  void saveSettings({ prefer_system_audio: next }, "prefer_system_audio")
                }
              />
            </Row>
            <Row
              label="Notes while the meeting runs"
              hint={interimHint(settings)}
            >
              {meetingSettings.savedKey === "interim_notes" && (
                <span className="kea-saved">Saved ✓</span>
              )}
              <Toggle
                label="Notes while the meeting runs"
                checked={settings.interim_notes ?? false}
                disabled={meetingSettings.busy}
                onChange={(next) => void saveSettings({ interim_notes: next }, "interim_notes")}
              />
            </Row>
            <Row
              label="Name meetings from your calendar"
              hint="Uses the title of the calendar event you were in. Your calendar is read on this Mac and never sent to an AI provider."
            >
              {meetingSettings.savedKey === "calendar_titles" && (
                <span className="kea-saved">Saved ✓</span>
              )}
              {settings.calendar_titles && calendarPerm !== "Granted" && (
                <button
                  type="button"
                  className="kea-btn"
                  onClick={() => void requestCalendar()}
                  disabled={actionBusy}
                >
                  Grant access
                </button>
              )}
              <Toggle
                label="Name meetings from your calendar"
                checked={settings.calendar_titles ?? false}
                disabled={meetingSettings.busy}
                onChange={(next) => {
                  void saveSettings({ calendar_titles: next }, "calendar_titles");
                  // Asking on the toggle rather than on the first recording: the
                  // prompt is the point at which the user has said yes to this,
                  // and interrupting a meeting to ask would be the opposite.
                  if (next && calendarPerm !== "Granted") void requestCalendar();
                }}
              />
            </Row>
            <Row label="Separate speakers by voice" hint={VOICE_HINT}>
              {meetingSettings.savedKey === "voice_separation" && (
                <span className="kea-saved">Saved ✓</span>
              )}
              {voiceEnablePending && !voiceModelsReady && (
                <button
                  type="button"
                  className="kea-btn"
                  onClick={() => void downloadVoiceModels()}
                  disabled={voiceDownloading || meetingSettings.busy}
                >
                  {voiceDownloading ? "Downloading…" : "Download speaker models"}
                </button>
              )}
              <Toggle
                label="Separate speakers by voice"
                checked={settings.voice_separation ?? false}
                disabled={meetingSettings.busy}
                onChange={(next) => void toggleVoiceSeparation(next)}
              />
            </Row>
            {voiceEnablePending && !voiceModelsReady && (
              <p className="kea-muted" role="status" style={{ margin: "0 0 12px", fontSize: "0.8125rem" }}>
                Turning this on needs two speaker models, which are not installed yet.
                {" "}Download them and voice separation turns on when they are ready.
              </p>
            )}
            {voiceStatus && (
              <p className="kea-muted" role="alert" style={{ margin: "0 0 12px", fontSize: "0.8125rem" }}>
                {voiceStatus}
              </p>
            )}
            <Row
              label="Transcribe every"
              hint="How often live transcript segments appear. Applies from the next meeting."
            >
              {meetingSettings.savedKey === "segment_duration_secs" && (
                <span className="kea-saved">Saved ✓</span>
              )}
              <input
                className="kea-input"
                type="number"
                aria-label="Seconds per transcript segment"
                min={5}
                max={120}
                step={5}
                value={settings.segment_duration_secs}
                disabled={meetingSettings.busy}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  if (!isNaN(v)) onSegmentDurationChange(v);
                }}
                onBlur={(e) => {
                  const v = parseInt(e.target.value, 10);
                  if (isNaN(v)) return;
                  // Clamp on persist: the backend takes any u32, so keep the
                  // value inside the advisory 5-120s range (negatives would
                  // fail u32 deserialization outright).
                  const clamped = Math.min(120, Math.max(5, v));
                  void saveSettings({ segment_duration_secs: clamped }, "segment_duration_secs");
                }}
                style={{ width: 88 }}
              />
              <span className="kea-muted">seconds</span>
            </Row>
          </RowGroup>
          {meetingSettings.error && (
            <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
              {meetingSettings.error}
            </p>
          )}
        </section>

        {checkSection}

        <FeatureAiCard ai={ai} featureLabel="Meetings" />

        <NotionSettings onStatusChange={onNotionStatusChange} />
      </div>
    </>
  );
}
