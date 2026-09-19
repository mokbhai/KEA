import { useCallback, useEffect, useRef, useState } from "react";
import {
  addMeetingActionItem,
  deleteMeeting,
  exportMeetingMarkdown,
  exportMeetingToNotion,
  getMeeting,
  getMeetingSettings,
  getMeetingState,
  getPermissionStatus,
  getSystemAudioCapability,
  listMeetings,
  meetingMarkdown,
  onMeetingError,
  onMeetingLevel,
  onMeetingNotes,
  onMeetingSegment,
  onMeetingState,
  requestPermission,
  revealPath,
  setMeetingActionItemStatus,
  setMeetingSettings,
  setMeetingSpeakerName,
  setMeetingTitle,
  startMeeting,
  stopMeeting,
  type ActionItemStatus,
  type Meeting,
  type MeetingDetail,
  type MeetingSegmentEvent,
  type MeetingNotes,
  type MeetingSettings,
  type MeetingState,
  type NotionStatus,
  type PermStatus,
  type SystemAudioCapability,
} from "../api";
import Banner from "../components/Banner";
import FeatureAiCard from "../components/FeatureAiCard";
import FeatureBanner from "../components/FeatureBanner";
import HotkeyRow from "../components/HotkeyRow";
import LevelMeter from "../components/LevelMeter";
import LoadingBlock from "../components/LoadingBlock";
import MeetingDetailView from "../components/MeetingDetail";
import NotionSettings from "../components/NotionSettings";
import { Row, RowGroup } from "../components/SettingsRow";
import Toggle from "../components/Toggle";
import TranscriptPanel, {
  speakerDisplayName,
  type TranscriptSegment,
} from "../components/TranscriptPanel";
import { useFeatureAi } from "../hooks/useFeatureAi";
import { useOptimisticSetting } from "../hooks/useOptimisticSetting";
import type { SlotSpec } from "../lib/featureSlot";
import { toMessage } from "../lib/format";
import type { Navigate } from "../lib/nav";

const MEETINGS_FEATURE = "meetings";
const MEETINGS_COMMAND = "toggle_meeting";

/** How long the "Try it" capture runs before it stops itself. */
const TEST_CAPTURE_MS = 10_000;

const SLOTS: SlotSpec[] = [
  { feature: "meetings", slot: "stt", capability: "stt", label: "Speech to text" },
  { feature: "meetings", slot: "llm", capability: "llm", label: "Notes writing" },
];

const capabilityLabels: Record<SystemAudioCapability, string> = {
  unavailable: "Mic only (system audio unavailable)",
  mic_only: "Mic only",
  loopback_device: "Mic + system (loopback device)",
  screen_capture_kit: "Mic + system (ScreenCaptureKit)",
};

type Props = {
  onNavigate?: Navigate;
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
 * What the page shows before the first `get_meeting_settings` resolves.
 *
 * Mirrors `MeetingSettings::default()` in
 * `crates/core/src/meetings/settings.rs`: both cost-spending features are off,
 * so a page that renders for a frame before the read lands never shows them on.
 */
const SETTINGS_PLACEHOLDER: MeetingSettings = {
  segment_duration_secs: 30,
  prefer_system_audio: true,
  interim_notes: false,
  interim_every_segments: 8,
  interim_every_minutes: 5,
  calendar_titles: false,
};

export default function MeetingsPage({ onNavigate }: Props) {
  const ai = useFeatureAi(SLOTS);
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<MeetingDetail | null>(null);
  const [listStatus, setListStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Fed by the Notion card below, which is the only thing that reads or writes
  // that setup; the page only needs to know whether to offer the button.
  const [notionStatus, setNotionStatus] = useState<NotionStatus | null>(null);

  const [state, setState] = useState<MeetingState>("idle");
  const [segments, setSegments] = useState<TranscriptSegment[]>([]);
  const [level, setLevel] = useState(0);
  const [capability, setCapability] = useState<SystemAudioCapability>("mic_only");
  const [screenPerm, setScreenPerm] = useState<PermStatus>("Unknown");
  const [meetingStatus, setMeetingStatus] = useState<string | null>(null);
  const [meetingBusy, setMeetingBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const testTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const [liveNotes, setLiveNotes] = useState<MeetingNotes | null>(null);
  const [calendarPerm, setCalendarPerm] = useState<PermStatus>("Unknown");

  const meetingSettings = useOptimisticSetting<MeetingSettings>({
    initial: SETTINGS_PLACEHOLDER,
    persist: setMeetingSettings,
    // A save writes the object whole, so re-reading first is what stops one
    // toggle from reverting a field another window or the API just changed.
    reread: getMeetingSettings,
  });
  const settings = meetingSettings.value;
  const { setValue: setSettings, setError: setSettingsError } = meetingSettings;
  // Set once the user edits a setting, so the mount fetch can't clobber input
  // typed before it resolves.
  const settingsTouchedRef = useRef(false);

  const recording = state === "recording";
  const processing = state === "processing";

  // The test-capture timeout fires outside React's render cycle, so it needs
  // the live state rather than the value captured when it was scheduled.
  const stateRef = useRef<MeetingState>(state);
  stateRef.current = state;

  const saveSettings = (patch: Partial<MeetingSettings>, key: string) => {
    settingsTouchedRef.current = true;
    return meetingSettings.save(patch, key);
  };

  const refreshList = useCallback(async () => {
    try {
      const items = await listMeetings(50);
      setMeetings(items);
    } catch (e) {
      setListStatus(toMessage(e));
    }
  }, []);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    setBusy(true);
    setListStatus(null);
    getMeeting(selectedId)
      .then(setDetail)
      .catch((e) => {
        setDetail(null);
        setListStatus(toMessage(e));
      })
      .finally(() => setBusy(false));
  }, [selectedId]);

  useEffect(() => {
    void getSystemAudioCapability().then(setCapability);
    void getPermissionStatus("screen_recording").then(setScreenPerm);
    // Asked for, never requested: reading the status does not prompt, and the
    // prompt is what the toggle is for.
    getPermissionStatus("calendar").then(setCalendarPerm).catch(() => {});
  }, []);

  useEffect(() => {
    getMeetingState()
      .then((payload) => {
        setState(payload.state);
        if (payload.active_meeting_id && payload.state === "recording") {
          getMeeting(payload.active_meeting_id)
            .then((d) => {
              setSegments(
                d.segments.map((s) => ({
                  meeting_id: payload.active_meeting_id!,
                  sequence: s.sequence,
                  text: s.text,
                  start_offset_ms: s.start_offset_ms,
                  end_offset_ms: s.end_offset_ms,
                  speaker: speakerDisplayName(s.speaker_key, d.speakers),
                })),
              );
            })
            .catch(() => {});
        }
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    getMeetingSettings()
      .then((loaded) => {
        if (!settingsTouchedRef.current) setSettings(loaded);
      })
      .catch((e) => setSettingsError(toMessage(e)));
  }, [setSettings, setSettingsError]);

  useEffect(() => {
    const unsubs = Promise.all([
      onMeetingState((next: MeetingState) => {
        setState(next);
        if (next === "recording") {
          setSegments([]);
          // Last meeting's notes are not this meeting's notes.
          setLiveNotes(null);
        }
      }),
      onMeetingNotes(setLiveNotes),
      onMeetingSegment((seg: MeetingSegmentEvent) => {
        setSegments((prev) => {
          if (prev.some((s) => s.meeting_id === seg.meeting_id && s.sequence === seg.sequence)) return prev;
          // No speaker rows while recording — the meeting has not been
          // fetched yet — so the defaults stand in. A renamed side shows up
          // on the saved transcript, which is where renaming happens.
          return [...prev, { meeting_id: seg.meeting_id, sequence: seg.sequence, start_offset_ms: seg.start_offset_ms, end_offset_ms: seg.end_offset_ms, text: seg.text, speaker: speakerDisplayName(seg.speaker_key) }];
        });
      }),
      onMeetingLevel(setLevel),
      onMeetingError((message) => {
        // Capture failed: cancel the pending test stop so its rejection can't
        // overwrite the message that actually explains what went wrong.
        clearTimeout(testTimer.current);
        setTesting(false);
        setMeetingStatus(message);
      }),
    ]);

    return () => {
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, []);

  useEffect(() => {
    if (state !== "recording") {
      setLevel(0);
    }
  }, [state]);

  useEffect(() => () => clearTimeout(testTimer.current), []);

  const onMeetingStopped = async (meetingId: string) => {
    await refreshList();
    setSelectedId(meetingId);
  };

  /**
   * Renames one side of the open meeting, then re-reads it.
   *
   * The re-read is what makes every already-rendered segment pick the new name
   * up: the transcript stores a speaker *key*, and the name lives once on the
   * meeting rather than being copied onto each line.
   */
  const onRenameSpeaker = async (speakerKey: string, displayName: string) => {
    if (!selectedId) return;
    setBusy(true);
    try {
      await setMeetingSpeakerName(selectedId, speakerKey, displayName);
      setDetail(await getMeeting(selectedId));
    } catch (e) {
      setListStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  /**
   * Re-reads the open meeting after a write.
   *
   * Every action-item and title write goes through here for the same reason
   * the speaker rename does: the row is the source of truth, and re-reading is
   * cheaper than mirroring the backend's dedupe and prose-rendering rules in
   * the page.
   */
  const mutateMeeting = async (write: (id: string) => Promise<unknown>) => {
    if (!selectedId) return;
    setBusy(true);
    setListStatus(null);
    try {
      await write(selectedId);
      setDetail(await getMeeting(selectedId));
    } catch (e) {
      setListStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const onSetActionItemStatus = (id: number, status: ActionItemStatus) =>
    mutateMeeting(() => setMeetingActionItemStatus(id, status));

  const onAddActionItem = (text: string) =>
    mutateMeeting((meetingId) => addMeetingActionItem(meetingId, text));

  const onRenameTitle = (title: string) =>
    mutateMeeting((meetingId) => setMeetingTitle(meetingId, title));

  /**
   * Copy in the webview rather than through the Rust clipboard path.
   *
   * `kea_platform`'s clipboard is wired into the save/paste/restore state
   * machine built for text insertion; reusing it for a plain copy would drag
   * in the clipboard-restore decision and the change-count verification for no
   * benefit.
   */
  const onCopyMarkdown = async () => {
    if (!selectedId) return;
    setBusy(true);
    try {
      await navigator.clipboard.writeText(await meetingMarkdown(selectedId));
      setListStatus("Copied the meeting to the clipboard as Markdown.");
    } catch (e) {
      setListStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const onSaveMarkdown = async () => {
    if (!selectedId) return;
    setBusy(true);
    try {
      const path = await exportMeetingMarkdown(selectedId);
      setListStatus(`Saved to ${path}`);
      // Revealing it is what makes a fixed destination acceptable: the user
      // never has to know where Downloads is.
      await revealPath(path).catch(() => {});
    } catch (e) {
      setListStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  /**
   * Notion is offered only once both halves of its setup are saved.
   *
   * A "Send to Notion" button on an unconfigured app is a button whose only
   * behaviour is an error, and the fix for that error is two screens away.
   */
  const notionReady = Boolean(
    notionStatus?.has_token &&
      notionStatus.parent_page.trim() &&
      !notionStatus.parent_page_error,
  );

  const onExportNotion = async () => {
    if (!selectedId) return;
    setBusy(true);
    try {
      const url = await exportMeetingToNotion(selectedId);
      setListStatus(`Sent to Notion — ${url}`);
    } catch (e) {
      setListStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const onDelete = async (id: string) => {
    if (!window.confirm("Delete this meeting and its transcript?")) return;
    setBusy(true);
    setListStatus(null);
    try {
      await deleteMeeting(id);
      if (selectedId === id) {
        setSelectedId(null);
        setDetail(null);
      }
      await refreshList();
    } catch (e) {
      setListStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const requestScreenRecording = async () => {
    setMeetingBusy(true);
    setMeetingStatus(null);
    try {
      const result = await requestPermission("screen_recording");
      setScreenPerm(result);
      void getSystemAudioCapability().then(setCapability);
      setMeetingStatus(
        result === "Granted"
          ? "Screen Recording permission granted."
          : "Screen Recording permission not granted — check System Settings.",
      );
    } catch (e) {
      setMeetingStatus(toMessage(e));
    } finally {
      setMeetingBusy(false);
    }
  };

  const requestCalendar = async () => {
    setMeetingBusy(true);
    setMeetingStatus(null);
    try {
      const result = await requestPermission("calendar");
      setCalendarPerm(result);
      setMeetingStatus(
        result === "Granted"
          ? "Calendar access granted — meetings will be named after the event they happened during."
          : "Calendar access not granted — meetings keep their generated titles.",
      );
    } catch (e) {
      setMeetingStatus(toMessage(e));
    } finally {
      setMeetingBusy(false);
    }
  };

  const onStart = async () => {
    setMeetingBusy(true);
    setMeetingStatus(null);
    setSegments([]);
    try {
      await startMeeting();
      setMeetingStatus("Recording — speak into the mic.");
      return true;
    } catch (e) {
      setMeetingStatus(toMessage(e));
      return false;
    } finally {
      setMeetingBusy(false);
    }
  };

  const onStop = async () => {
    clearTimeout(testTimer.current);
    setMeetingBusy(true);
    setMeetingStatus(null);
    try {
      const saved = await stopMeeting();
      setMeetingStatus(`Meeting saved: ${saved.meeting.title}`);
      await onMeetingStopped(saved.meeting.id);
    } catch (e) {
      setMeetingStatus(toMessage(e));
    } finally {
      setMeetingBusy(false);
      setTesting(false);
    }
  };

  const runTestCapture = async () => {
    setTesting(true);
    const started = await onStart();
    if (!started) {
      setTesting(false);
      return;
    }
    setMeetingStatus("Test capture running — it stops itself in 10 seconds.");
    testTimer.current = setTimeout(() => {
      // start_meeting resolved, but the capture may have failed since; stopping
      // a meeting that is no longer recording would only replace the real
      // error with "no meeting is recording".
      if (stateRef.current !== "recording") {
        setTesting(false);
        return;
      }
      void onStop();
    }, TEST_CAPTURE_MS);
  };

  const needsScreenRecording =
    capability === "screen_capture_kit" && screenPerm !== "Granted";

  const actionBusy = meetingBusy || busy;

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>Meetings</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
          Record a meeting, watch the transcript appear live, and get notes when you
          stop.
        </p>
      </header>

      <FeatureBanner ai={ai} onNavigate={onNavigate} />

      {needsScreenRecording && (
        <Banner
          variant="warn"
          action={
            <button
              type="button"
              className="kea-btn"
              onClick={() => void requestScreenRecording()}
              disabled={actionBusy}
            >
              Grant permission
            </button>
          }
        >
          System audio — capturing the other side of a call needs Screen Recording
          permission.
        </Banner>
      )}

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
                settingsTouchedRef.current = true;
                const v = parseInt(e.target.value, 10);
                if (!isNaN(v)) setSettings({ ...settings, segment_duration_secs: v });
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

      <FeatureAiCard ai={ai} featureLabel="Meetings" />

      <NotionSettings onStatusChange={setNotionStatus} />

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Try it</h2>
        <div className="kea-card">
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 12,
              flexWrap: "wrap",
              marginBottom: 12,
            }}
          >
            <span className="kea-muted" style={{ fontSize: 13 }}>
              State:{" "}
              <strong style={{ color: "var(--text)" }}>
                {state === "idle" ? "Idle" : state === "recording" ? "Recording" : "Processing"}
              </strong>
            </span>
            {recording && <LevelMeter level={level} />}
          </div>

          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              type="button"
              className="kea-btn kea-btn--primary"
              onClick={() => void runTestCapture()}
              disabled={actionBusy || recording || processing || testing}
            >
              Run a 10-second test
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onStart()}
              disabled={actionBusy || recording || processing || testing}
            >
              Start meeting
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onStop()}
              disabled={actionBusy || !recording}
            >
              Stop meeting
            </button>
          </div>
          <p className="kea-muted" style={{ margin: "8px 0 0", fontSize: "0.8125rem" }}>
            The test is a real capture — it is saved to your meetings like any other.
          </p>
          {meetingStatus && (
            <p className="kea-muted" style={{ marginTop: 12, marginBottom: 0 }}>
              {meetingStatus}
            </p>
          )}
        </div>
      </section>

      {liveNotes && (
        <section style={{ marginBottom: 24 }}>
          <h2 style={{ margin: "0 0 12px" }}>Notes so far</h2>
          <div className="kea-card">
            {/*
              Said plainly, because a stale summary read as a final one is the
              way this feature misleads: the fold can compress out a point
              mentioned once, and the full-transcript pass at stop is what
              fixes it.
            */}
            <p className="kea-muted" style={{ margin: "0 0 8px", fontSize: "0.8125rem" }}>
              Updated every few minutes while the meeting runs. The full notes are
              written when you stop.
            </p>
            {liveNotes.summary.trim() && (
              <p style={{ margin: "0 0 8px", whiteSpace: "pre-wrap" }}>{liveNotes.summary}</p>
            )}
            {liveNotes.action_items.trim() && (
              <>
                <h3 style={{ margin: "0 0 4px", fontSize: 14 }}>Action items</h3>
                <p style={{ margin: 0, whiteSpace: "pre-wrap" }}>{liveNotes.action_items}</p>
              </>
            )}
          </div>
        </section>
      )}

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Live transcript</h2>
        <div className="kea-card">
          <TranscriptPanel
            segments={segments}
            live={recording}
            emptyMessage={
              recording
                ? "Listening — segments appear every few seconds…"
                : "Start a meeting to see live transcription."
            }
          />
        </div>
      </section>

      <div
        style={{
          display: "grid",
          gridTemplateColumns: "minmax(200px, 280px) 1fr",
          gap: 16,
          alignItems: "start",
        }}
      >
        <aside
          className="kea-card"
          style={{
            padding: 12,
            maxHeight: 480,
            overflowY: "auto",
          }}
        >
          <h2 style={{ margin: "0 0 12px", fontSize: 15 }}>Past meetings</h2>
          {meetings.length === 0 ? (
            <p className="kea-muted" style={{ margin: 0 }}>
              No meetings yet.
            </p>
          ) : (
            <ul style={{ margin: 0, padding: 0, listStyle: "none" }}>
              {meetings.map((m) => (
                <li key={m.id} style={{ marginBottom: 4 }}>
                  <button
                    type="button"
                    onClick={() => setSelectedId(m.id)}
                    style={{
                      width: "100%",
                      textAlign: "left",
                      padding: "8px 10px",
                      border:
                        selectedId === m.id
                          ? "2px solid var(--accent)"
                          : "1px solid var(--border)",
                      borderRadius: 6,
                      background:
                        selectedId === m.id ? "var(--surface-2)" : "var(--surface)",
                      color: "var(--text)",
                      cursor: "pointer",
                      fontFamily: "inherit",
                    }}
                  >
                    <div style={{ fontWeight: 600, fontSize: 13 }}>{m.title}</div>
                    <div className="kea-muted" style={{ fontSize: 11, marginTop: 2 }}>
                      {m.started_at} · {m.status}
                    </div>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </aside>

        <section className="kea-card" style={{ minHeight: 200 }}>
          {selectedId && detail ? (
            <MeetingDetailView
              detail={detail}
              onDelete={onDelete}
              busy={busy}
              onRenameSpeaker={onRenameSpeaker}
              onRenameTitle={onRenameTitle}
              onSetActionItemStatus={onSetActionItemStatus}
              onAddActionItem={onAddActionItem}
              onCopyMarkdown={onCopyMarkdown}
              onSaveMarkdown={onSaveMarkdown}
              onExportNotion={notionReady ? onExportNotion : undefined}
            />
          ) : selectedId && busy ? (
            <LoadingBlock label="Loading meeting…" />
          ) : (
            <p className="kea-muted" style={{ margin: 0 }}>
              Select a meeting to view notes and transcript.
            </p>
          )}
        </section>
      </div>

      {listStatus && (
        <p style={{ marginTop: 12, fontSize: 13, color: "var(--danger)" }}>
          {listStatus}
        </p>
      )}
    </div>
  );
}
