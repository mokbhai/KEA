import { useCallback, useEffect, useRef, useState } from "react";
import {
  deleteMeeting,
  getMeeting,
  getMeetingSettings,
  getMeetingState,
  getPermissionStatus,
  getSystemAudioCapability,
  listMeetings,
  onMeetingError,
  onMeetingLevel,
  onMeetingSegment,
  onMeetingState,
  requestPermission,
  setMeetingSettings,
  startMeeting,
  stopMeeting,
  type Meeting,
  type MeetingDetail,
  type MeetingSegmentEvent,
  type MeetingSettings,
  type MeetingState,
  type PermStatus,
  type SystemAudioCapability,
} from "../api";
import Banner from "../components/Banner";
import FeatureAiCard from "../components/FeatureAiCard";
import FeatureBanner, { type FixNavigate } from "../components/FeatureBanner";
import HotkeyRow from "../components/HotkeyRow";
import LevelMeter from "../components/LevelMeter";
import MeetingDetailView from "../components/MeetingDetail";
import { Row, RowGroup } from "../components/SettingsRow";
import Spinner from "../components/Spinner";
import Toggle from "../components/Toggle";
import TranscriptPanel, { type TranscriptSegment } from "../components/TranscriptPanel";
import { useFeatureAi } from "../hooks/useFeatureAi";
import { useSavedFlash } from "../hooks/useSavedFlash";
import type { SlotSpec } from "../lib/featureSlot";

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
  onNavigate?: FixNavigate;
};

export default function MeetingsPage({ onNavigate }: Props) {
  const ai = useFeatureAi(SLOTS);
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<MeetingDetail | null>(null);
  const [listStatus, setListStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [state, setState] = useState<MeetingState>("idle");
  const [segments, setSegments] = useState<TranscriptSegment[]>([]);
  const [level, setLevel] = useState(0);
  const [capability, setCapability] = useState<SystemAudioCapability>("mic_only");
  const [screenPerm, setScreenPerm] = useState<PermStatus>("Unknown");
  const [meetingStatus, setMeetingStatus] = useState<string | null>(null);
  const [meetingBusy, setMeetingBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const testTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const [settings, setSettings] = useState<MeetingSettings>({
    segment_duration_secs: 30,
    prefer_system_audio: true,
  });
  const [settingsStatus, setSettingsStatus] = useState<string | null>(null);
  const [settingsBusy, setSettingsBusy] = useState(false);
  const [savedKey, flash] = useSavedFlash();
  // Set once the user edits a setting, so the mount fetch can't clobber input
  // typed before it resolves.
  const settingsTouchedRef = useRef(false);

  const recording = state === "recording";
  const processing = state === "processing";

  // The test-capture timeout fires outside React's render cycle, so it needs
  // the live state rather than the value captured when it was scheduled.
  const stateRef = useRef<MeetingState>(state);
  stateRef.current = state;

  const saveSettings = async (next: MeetingSettings, key: string) => {
    settingsTouchedRef.current = true;
    setSettings(next);
    setSettingsBusy(true);
    setSettingsStatus(null);
    try {
      await setMeetingSettings(next);
      flash(key);
    } catch (e) {
      setSettingsStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setSettingsBusy(false);
    }
  };

  const refreshList = useCallback(async () => {
    try {
      const items = await listMeetings(50);
      setMeetings(items);
    } catch (e) {
      setListStatus(e instanceof Error ? e.message : String(e));
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
        setListStatus(e instanceof Error ? e.message : String(e));
      })
      .finally(() => setBusy(false));
  }, [selectedId]);

  useEffect(() => {
    void getSystemAudioCapability().then(setCapability);
    void getPermissionStatus("screen_recording").then(setScreenPerm);
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
      .catch((e) => setSettingsStatus(e instanceof Error ? e.message : String(e)));
  }, []);

  useEffect(() => {
    const unsubs = Promise.all([
      onMeetingState((next: MeetingState) => {
        setState(next);
        if (next === "recording") {
          setSegments([]);
        }
      }),
      onMeetingSegment((seg: MeetingSegmentEvent) => {
        setSegments((prev) => {
          if (prev.some((s) => s.meeting_id === seg.meeting_id && s.sequence === seg.sequence)) return prev;
          return [...prev, { meeting_id: seg.meeting_id, sequence: seg.sequence, start_offset_ms: seg.start_offset_ms, end_offset_ms: seg.end_offset_ms, text: seg.text }];
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
      setListStatus(e instanceof Error ? e.message : String(e));
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
      setMeetingStatus(e instanceof Error ? e.message : String(e));
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
      setMeetingStatus(e instanceof Error ? e.message : String(e));
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
      setMeetingStatus(e instanceof Error ? e.message : String(e));
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
            {savedKey === "prefer_system_audio" && <span className="kea-saved">Saved ✓</span>}
            <Toggle
              label="Record system audio too"
              checked={settings.prefer_system_audio}
              disabled={settingsBusy}
              onChange={(next) =>
                void saveSettings({ ...settings, prefer_system_audio: next }, "prefer_system_audio")
              }
            />
          </Row>
          <Row
            label="Transcribe every"
            hint="How often live transcript segments appear. Applies from the next meeting."
          >
            {savedKey === "segment_duration_secs" && <span className="kea-saved">Saved ✓</span>}
            <input
              className="kea-input"
              type="number"
              aria-label="Seconds per transcript segment"
              min={5}
              max={120}
              step={5}
              value={settings.segment_duration_secs}
              disabled={settingsBusy}
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
                void saveSettings(
                  { ...settings, segment_duration_secs: clamped },
                  "segment_duration_secs",
                );
              }}
              style={{ width: 88 }}
            />
            <span className="kea-muted">seconds</span>
          </Row>
        </RowGroup>
        {settingsStatus && (
          <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
            {settingsStatus}
          </p>
        )}
      </section>

      <FeatureAiCard ai={ai} featureLabel="Meetings" />

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
            <MeetingDetailView detail={detail} onDelete={onDelete} busy={busy} />
          ) : selectedId && busy ? (
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <Spinner size={16} />
              <span className="kea-muted" style={{ margin: 0 }}>
                Loading meeting…
              </span>
            </div>
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
