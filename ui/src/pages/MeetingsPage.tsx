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
import LevelMeter from "../components/LevelMeter";
import MeetingDetailView from "../components/MeetingDetail";
import HotkeyBinder from "../components/HotkeyBinder";
import SlotBinder from "../components/SlotBinder";
import Spinner from "../components/Spinner";
import TranscriptPanel, { type TranscriptSegment } from "../components/TranscriptPanel";

const MEETINGS_FEATURE = "meetings";
const MEETINGS_STT_SLOT = "stt";
const MEETINGS_LLM_SLOT = "llm";

const capabilityLabels: Record<SystemAudioCapability, string> = {
  unavailable: "Mic only (system audio unavailable)",
  mic_only: "Mic only",
  loopback_device: "Mic + system (loopback device)",
  screen_capture_kit: "Mic + system (ScreenCaptureKit)",
};

const settingsSummaryStyle: React.CSSProperties = {
  cursor: "pointer",
  color: "var(--text)",
  fontWeight: 600,
  fontSize: "0.875rem",
};

export default function MeetingsPage() {
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

  const [settings, setSettings] = useState<MeetingSettings>({
    segment_duration_secs: 30,
    prefer_system_audio: true,
  });
  const [settingsStatus, setSettingsStatus] = useState<string | null>(null);
  const [settingsBusy, setSettingsBusy] = useState(false);
  // Set once the user edits a setting, so the mount fetch can't clobber input
  // typed before it resolves.
  const settingsTouchedRef = useRef(false);

  const recording = state === "recording";
  const processing = state === "processing";

  const saveSettings = async (next: MeetingSettings) => {
    settingsTouchedRef.current = true;
    setSettings(next);
    setSettingsBusy(true);
    setSettingsStatus(null);
    try {
      await setMeetingSettings(next);
      setSettingsStatus("Meeting settings saved.");
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
    } catch (e) {
      setMeetingStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setMeetingBusy(false);
    }
  };

  const onStop = async () => {
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
    }
  };

  const needsScreenRecording =
    capability === "screen_capture_kit" && screenPerm !== "Granted";

  const actionBusy = meetingBusy || busy;

  return (
    <div>
      <header>
        <h2 style={{ marginTop: 0 }}>Meetings</h2>
        <p className="kea-muted" style={{ marginTop: 0 }}>
          Capture meeting audio, stream live transcript segments, and synthesize
          notes when you stop.
        </p>
      </header>

      <section style={{ marginBottom: 24 }}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 12,
            flexWrap: "wrap",
            marginBottom: 12,
          }}
        >
          <span
            style={{
              fontSize: 12,
              fontWeight: 600,
              padding: "4px 10px",
              borderRadius: 999,
              background: "var(--surface-2)",
              color: "var(--accent)",
              border: "1px solid var(--border)",
            }}
          >
            {capabilityLabels[capability]}
          </span>
          <span className="kea-muted" style={{ fontSize: 13 }}>
            State:{" "}
            <strong style={{ color: "var(--text)" }}>
              {state === "idle"
                ? "Idle"
                : state === "recording"
                  ? "Recording"
                  : "Processing"}
            </strong>
          </span>
          {recording && <LevelMeter level={level} />}
        </div>

        {needsScreenRecording && (
          <div style={{ marginBottom: 12 }}>
            <p className="kea-muted" style={{ margin: "0 0 8px" }}>
              System audio capture requires Screen Recording permission on macOS.
            </p>
            <button
              type="button"
              className="kea-btn"
              onClick={requestScreenRecording}
              disabled={actionBusy}
            >
              Request Screen Recording permission
            </button>
          </div>
        )}

        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <button
            type="button"
            className="kea-btn kea-btn--primary"
            onClick={onStart}
            disabled={actionBusy || recording || processing}
          >
            Start meeting
          </button>
          <button
            type="button"
            className="kea-btn"
            onClick={onStop}
            disabled={actionBusy || !recording}
          >
            Stop meeting
          </button>
        </div>
        {meetingStatus && (
          <p className="kea-muted" style={{ marginTop: 12, marginBottom: 0 }}>
            {meetingStatus}
          </p>
        )}
      </section>

      <details open className="kea-card" style={{ marginBottom: 16 }}>
        <summary className="kea-label" style={settingsSummaryStyle}>
          Settings
        </summary>
        <div style={{ marginTop: 16 }}>
          <section className="kea-card" style={{ marginBottom: 16 }}>
            <h3 style={{ margin: "0 0 12px" }}>Meeting options</h3>
            <label style={{ display: "block", marginBottom: 12 }}>
              <span className="kea-label">Segment duration (seconds)</span>
              <input
                className="kea-input"
                type="number"
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
                  saveSettings({ ...settings, segment_duration_secs: clamped });
                }}
                style={{ maxWidth: 120 }}
              />
              <span className="kea-muted" style={{ display: "block", marginTop: 4 }}>
                Applies from the next meeting.
              </span>
            </label>
            <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 12 }}>
              <input
                type="checkbox"
                checked={settings.prefer_system_audio}
                disabled={settingsBusy}
                onChange={(e) =>
                  saveSettings({ ...settings, prefer_system_audio: e.target.checked })
                }
              />
              <span>Prefer system audio</span>
            </label>
            {settingsStatus && (
              <p className="kea-muted" style={{ marginTop: 0, marginBottom: 0 }}>
                {settingsStatus}
              </p>
            )}
          </section>
          <HotkeyBinder
            feature={MEETINGS_FEATURE}
            command="toggle_meeting"
            label="Meeting hotkey"
          />
          <SlotBinder
            feature={MEETINGS_FEATURE}
            slot={MEETINGS_STT_SLOT}
            slotKind="stt"
            title="Meetings STT slot"
          />
          <SlotBinder
            feature={MEETINGS_FEATURE}
            slot={MEETINGS_LLM_SLOT}
            slotKind="llm"
            title="Meetings LLM slot"
          />
        </div>
      </details>

      <section className="kea-card" style={{ marginBottom: 24 }}>
        <h3 style={{ margin: "0 0 8px" }}>Live transcript</h3>
        <TranscriptPanel
          segments={segments}
          live={recording}
          emptyMessage={
            recording
              ? "Listening — segments appear every few seconds…"
              : "Start a meeting to see live transcription."
          }
        />
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
          <h3 style={{ margin: "0 0 12px", fontSize: 15 }}>Past meetings</h3>
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
