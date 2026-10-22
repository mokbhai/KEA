import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  addMeetingActionItem,
  deleteMeeting,
  downloadOnnxModel,
  exportMeetingMarkdown,
  exportMeetingToNotion,
  getMeeting,
  getMeetingSettings,
  getMeetingState,
  getPermissionStatus,
  getSystemAudioCapability,
  listInstalledOnnxModels,
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
  type CaptureMode,
  type Meeting,
  type MeetingDetail,
  type MeetingNotes,
  type MeetingSegmentEvent,
  type MeetingSettings,
  type MeetingState,
  type NotionStatus,
  type PermStatus,
  type SystemAudioCapability,
} from "../api";
import Banner from "../components/Banner";
import FeatureBanner from "../components/FeatureBanner";
import LevelMeter from "../components/LevelMeter";
import LoadingBlock from "../components/LoadingBlock";
import MeetingSettingsPanel from "../components/meetings/MeetingSettingsPanel";
import MeetingList from "../components/meetings/MeetingList";
import MeetingPane, { type MeetingPaneHandle } from "../components/meetings/MeetingPane";
import { speakerDisplayName, type TranscriptSegment } from "../components/TranscriptPanel";
import { useFeatureAi } from "../hooks/useFeatureAi";
import { useMediaQuery, MEETINGS_NARROW_QUERY } from "../hooks/useMediaQuery";
import { useModelDownloads } from "../hooks/useModelDownloads";
import { useOptimisticSetting } from "../hooks/useOptimisticSetting";
import type { SlotSpec } from "../lib/featureSlot";
import { toMessage } from "../lib/format";
import type { Navigate } from "../lib/nav";

/**
 * The two speaker models voice separation needs, by their catalog ids. Both
 * are required even though only the embedding model runs per window, so the
 * toggle treats diarization as one bundle the user downloads once.
 */
const DIARIZATION_MODEL_IDS = [
  "pyannote-segmentation-3-0",
  "campplus-sv-en-voxceleb-16k",
] as const;
const VOICE_MODEL_SET: Set<string> = new Set(DIARIZATION_MODEL_IDS);

/** How long the "Try it" capture runs before it stops itself. */
const TEST_CAPTURE_MS = 10_000;

const SLOTS: SlotSpec[] = [
  { feature: "meetings", slot: "stt", capability: "stt", label: "Speech to text" },
  { feature: "meetings", slot: "llm", capability: "llm", label: "Notes writing" },
];

type Props = {
  onNavigate?: Navigate;
  /**
   * The selected meeting, lifted into the app shell so it survives leaving the
   * page. Omitted, the page keeps its own selection (used by tests).
   */
  selectedMeetingId?: string | null;
  onSelectMeeting?: (id: string | null) => void;
};

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
  voice_separation: false,
};

/** The meeting that is recording, before the list has caught up with it. */
function recordingPlaceholder(
  id: string,
  startedAt: string,
  title: string,
  captureMode: CaptureMode,
): Meeting {
  return {
    id,
    title,
    started_at: startedAt,
    ended_at: null,
    status: "recording",
    capture_mode: captureMode,
    stt_engine_id: null,
    llm_engine_id: null,
    error: null,
    title_source: "llm",
  };
}

/** The most recently started meeting, or null for none. */
function newestMeetingId(list: Meeting[]): string | null {
  let best: Meeting | null = null;
  for (const m of list) {
    if (!best || Date.parse(m.started_at) > Date.parse(best.started_at)) best = m;
  }
  return best?.id ?? null;
}

export default function MeetingsPage({ onNavigate, selectedMeetingId, onSelectMeeting }: Props) {
  const ai = useFeatureAi(SLOTS);
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [listLoaded, setListLoaded] = useState(false);
  const [localSelectedId, setLocalSelectedId] = useState<string | null>(null);
  const selectionIsControlled = selectedMeetingId !== undefined;
  const selectedId = selectionIsControlled ? selectedMeetingId : localSelectedId;
  const setSelectedId = useCallback(
    (id: string | null) => {
      if (selectionIsControlled) onSelectMeeting?.(id);
      else setLocalSelectedId(id);
    },
    [selectionIsControlled, onSelectMeeting],
  );

  const [detail, setDetail] = useState<MeetingDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const detailRequest = useRef(0);
  const [listStatus, setListStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Narrow windows show one pane at a time; this is which one is showing.
  const narrow = useMediaQuery(MEETINGS_NARROW_QUERY);
  const [showDetailNarrow, setShowDetailNarrow] = useState(false);
  // Fed by the Notion card below, which is the only thing that reads or writes
  // that setup; the page only needs to know whether to offer the button.
  const [notionStatus, setNotionStatus] = useState<NotionStatus | null>(null);
  const paneRef = useRef<MeetingPaneHandle>(null);

  const [state, setState] = useState<MeetingState>("idle");
  // The meeting the backend is recording or finishing, once its id is known.
  const [activeMeetingId, setActiveMeetingId] = useState<string | null>(null);
  // Read by the state listener, which is subscribed once and would otherwise
  // see the id from its first render.
  const activeMeetingRef = useRef<string | null>(null);
  activeMeetingRef.current = activeMeetingId;
  const [recordingSince, setRecordingSince] = useState(() => new Date().toISOString());
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
  // Ids of the diarization models already on disk. Null until the first read
  // lands, so the toggle does not offer a download for models it has not
  // checked yet.
  const [installedVoiceModels, setInstalledVoiceModels] = useState<Set<string> | null>(null);
  // Set when the user asked for voice separation without the models: the
  // setting is only written once they are all here, so it never reads "on" for
  // a feature that cannot run.
  const [voiceEnablePending, setVoiceEnablePending] = useState(false);
  const [voiceStatus, setVoiceStatus] = useState<string | null>(null);

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
  const onSegmentDurationChange = (seconds: number) => {
    settingsTouchedRef.current = true;
    setSettings({ ...settings, segment_duration_secs: seconds });
  };
  // The panel's state lives here, so closing it only hides it. Focus goes back
  // to the ⚙ that opened it, whichever way it was closed.
  const [settingsOpen, setSettingsOpen] = useState(false);
  const settingsButtonRef = useRef<HTMLButtonElement>(null);
  const closeSettings = useCallback(() => {
    setSettingsOpen(false);
    settingsButtonRef.current?.focus();
  }, []);

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

  const missingVoiceModels = DIARIZATION_MODEL_IDS.filter(
    (id) => !installedVoiceModels?.has(id),
  );
  const voiceModelsReady = installedVoiceModels !== null && missingVoiceModels.length === 0;

  const refreshVoiceModels = useCallback(async (): Promise<Set<string>> => {
    const ids = new Set(await listInstalledOnnxModels("diarization"));
    setInstalledVoiceModels(ids);
    return ids;
  }, []);

  // A download finishing is the only moment a pending enable can complete, so
  // the check runs from the download event rather than on a timer. The pending
  // flag is read through a ref because the event listener holds the render it
  // was registered in.
  const voiceEnablePendingRef = useRef(voiceEnablePending);
  voiceEnablePendingRef.current = voiceEnablePending;
  const { progressById: voiceProgress } = useModelDownloads({
    catalogIds: VOICE_MODEL_SET,
    onComplete: () => {
      refreshVoiceModels()
        .then((ids) => {
          if (!voiceEnablePendingRef.current) return;
          if (DIARIZATION_MODEL_IDS.every((id) => ids.has(id))) {
            setVoiceEnablePending(false);
            setVoiceStatus(null);
            void saveSettings({ voice_separation: true }, "voice_separation");
          }
        })
        .catch((e) => setVoiceStatus(toMessage(e)));
    },
    onError: (modelId, message) => setVoiceStatus(`${modelId}: ${message}`),
  });
  const voiceDownloading = DIARIZATION_MODEL_IDS.some((id) => voiceProgress.has(id));

  const toggleVoiceSeparation = async (next: boolean) => {
    setVoiceStatus(null);
    if (!next) {
      setVoiceEnablePending(false);
      void saveSettings({ voice_separation: false }, "voice_separation");
      return;
    }
    // The first read of the installed models may not have landed yet; "not
    // known" must not be taken for "missing", or an installed pair would be
    // offered for download again.
    let installed = installedVoiceModels;
    if (installed === null) {
      try {
        installed = await refreshVoiceModels();
      } catch (e) {
        setVoiceStatus(toMessage(e));
        return;
      }
    }
    if (DIARIZATION_MODEL_IDS.every((id) => installed.has(id))) {
      void saveSettings({ voice_separation: true }, "voice_separation");
      return;
    }
    // Models missing: offer the download and enable once they land, instead of
    // saving a setting that would silently do nothing at the next meeting.
    setVoiceEnablePending(true);
  };

  const downloadVoiceModels = async () => {
    setVoiceStatus(null);
    try {
      for (const id of missingVoiceModels) {
        await downloadOnnxModel("diarization", id);
      }
    } catch (e) {
      setVoiceStatus(toMessage(e));
    }
  };

  const refreshList = useCallback(async () => {
    try {
      const items = await listMeetings(50);
      setMeetings(items);
    } catch (e) {
      setListStatus(toMessage(e));
    } finally {
      setListLoaded(true);
    }
  }, []);

  /**
   * Loads one meeting's detail. A newer request wins, so a slow answer for a
   * meeting the user has already left cannot overwrite the one on screen.
   */
  /**
   * Reads one meeting's detail. Every read goes through here, with the
   * request counter, so a slow answer for a meeting the user has since left
   * cannot overwrite the one now open. `quiet` skips the loading state, for a
   * re-read after a write where the pane should not flash.
   */
  const loadDetail = useCallback(async (id: string, quiet = false) => {
    const request = ++detailRequest.current;
    if (!quiet) setDetailLoading(true);
    try {
      const next = await getMeeting(id);
      if (request === detailRequest.current) setDetail(next);
    } catch (e) {
      if (request === detailRequest.current) {
        setDetail(null);
        setListStatus(toMessage(e));
      }
    } finally {
      if (request === detailRequest.current) setDetailLoading(false);
    }
  }, []);

  // Read by applyDetail, which can be called after the selection has moved on.
  const selectedIdRef = useRef<string | null>(null);
  selectedIdRef.current = selectedId;

  /**
   * Takes a fresh detail that a notes write returned (apply, edit, undo). It
   * goes through the same request counter as loadDetail, so a read still in
   * flight for this meeting cannot land after it and put back older notes, and
   * a reply for a meeting the user has left is dropped.
   */
  const applyDetail = useCallback((next: MeetingDetail) => {
    if (next.meeting.id !== selectedIdRef.current) return;
    detailRequest.current++;
    setDetail(next);
    // The read this supersedes can no longer clear its own loading flag.
    setDetailLoading(false);
  }, []);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  // The recording meeting's row, from the list if it is there yet, else a
  // placeholder built from what the live events have told us.
  const listedActive = activeMeetingId
    ? (meetings.find((m) => m.id === activeMeetingId) ?? null)
    : null;
  const activeRow: Meeting | null =
    activeMeetingId && (recording || processing)
      ? (listedActive ??
        recordingPlaceholder(
          activeMeetingId,
          recordingSince,
          processing ? "Finishing meeting…" : "Recording…",
          settings.prefer_system_audio && capability !== "mic_only" && capability !== "unavailable"
            ? "mic_and_system"
            : "mic_only",
        ))
      : null;
  // The pinned row is the one being recorded right now; while it finishes it
  // sits in the list like any other.
  const pinnedRow = recording ? activeRow : null;
  const listRows = pinnedRow ? meetings.filter((m) => m.id !== pinnedRow.id) : meetings;
  const live = recording && activeMeetingId !== null && selectedId === activeMeetingId;
  const generating = processing && activeMeetingId !== null && selectedId === activeMeetingId;

  // Whether the Ask AI tab can send: the notes-writing LLM is bound and usable.
  // null while the first status check is still running.
  const llmStatus = ai.statuses?.find((s) => s.spec.slot === "llm") ?? null;
  const aiReady: boolean | null = ai.statuses === null ? null : !llmStatus?.blocked;
  const aiBlockedReason = llmStatus?.blocked?.message ?? null;

  // The header reads the list row, or the fetched meeting itself when the list
  // has not caught up with a meeting that just stopped.
  const paneMeeting: Meeting | null = !selectedId
    ? null
    : selectedId === activeRow?.id
      ? activeRow
      : (meetings.find((m) => m.id === selectedId) ??
        (detail && detail.meeting.id === selectedId ? detail.meeting : null));

  // Opening on a meeting that is no longer in the list (deleted, or a stale
  // session value) falls back to the default instead of showing nothing.
  const knownSelection =
    selectedId !== null &&
    (selectedId === activeRow?.id || meetings.some((m) => m.id === selectedId));

  useEffect(() => {
    if (!listLoaded || knownSelection) return;
    const next = recording && activeMeetingId ? activeMeetingId : newestMeetingId(meetings);
    if (next !== selectedId) setSelectedId(next);
  }, [listLoaded, knownSelection, recording, activeMeetingId, meetings, selectedId, setSelectedId]);

  // A meeting that starts recording becomes the selection, so its live view is
  // what the user sees. Keyed on the id and the state only, so a selection the
  // user makes during the meeting is left alone.
  useEffect(() => {
    if (recording && activeMeetingId) setSelectedId(activeMeetingId);
  }, [recording, activeMeetingId, setSelectedId]);

  // Loads the open meeting's detail, except while it is the live one, whose
  // view comes from events. Stopping flips it back to a stored meeting, so the
  // detail is read again then.
  useEffect(() => {
    if (!selectedId) {
      detailRequest.current++;
      setDetail(null);
      setDetailLoading(false);
      return;
    }
    if (live) return;
    void loadDetail(selectedId);
  }, [selectedId, live, loadDetail]);

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
          setActiveMeetingId(payload.active_meeting_id);
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
    refreshVoiceModels().catch(() => setInstalledVoiceModels(new Set()));
  }, [refreshVoiceModels]);

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
        // Finishing ends in idle however the stop was asked for — the button,
        // the hotkey, the tray or a script. Only here is the finished meeting
        // re-read for all of them; the button path alone used to do it, so a
        // hotkey stop left the pane saying no notes were written.
        if (next === "idle" && activeMeetingRef.current) {
          const finished = activeMeetingRef.current;
          void refreshList();
          void loadDetail(finished, true);
        }
        if (next === "recording") {
          setSegments([]);
          // Last meeting's notes are not this meeting's notes.
          setLiveNotes(null);
          // A new meeting: forget the last id until this one's arrives, and
          // show the list row for it as soon as the list has it.
          setActiveMeetingId(null);
          setRecordingSince(new Date().toISOString());
          void refreshList();
          getMeetingState()
            .then((payload) => {
              if (payload.active_meeting_id) setActiveMeetingId(payload.active_meeting_id);
            })
            .catch(() => {});
        }
      }),
      onMeetingNotes((notes) => {
        // Not gated on the recording state: a state event and the first notes
        // can land in one batch, and the selection only follows while recording.
        setActiveMeetingId(notes.meeting_id);
        setLiveNotes(notes);
      }),
      onMeetingSegment((seg: MeetingSegmentEvent) => {
        setActiveMeetingId(seg.meeting_id);
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
  }, [refreshList, loadDetail]);

  useEffect(() => {
    if (state !== "recording") {
      setLevel(0);
    }
  }, [state]);

  useEffect(() => () => clearTimeout(testTimer.current), []);

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
      await loadDetail(selectedId, true);
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
      await loadDetail(selectedId, true);
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

  // The header and the list row read the title from the list, so a rename
  // re-reads the list as well as the detail, or the old name would stand.
  const onRenameTitle = async (title: string) => {
    await mutateMeeting((meetingId) => setMeetingTitle(meetingId, title));
    await refreshList();
  };

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
        setShowDetailNarrow(false);
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
      const id = await startMeeting();
      setActiveMeetingId(id);
      void refreshList();
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
      await refreshList();
      setSelectedId(saved.meeting.id);
      // The same meeting may already be selected, in which case nothing else
      // reads its notes again: they were written while it finished.
      await loadDetail(saved.meeting.id);
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
  const recordDisabled = actionBusy || recording || processing || testing;

  const onSelectRow = (id: string) => {
    setSelectedId(id);
    setShowDetailNarrow(true);
  };

  const hasAnyMeeting = meetings.length > 0 || pinnedRow !== null;
  // Narrow: one pane at a time. With no meetings the empty state is all there is.
  const showListPane = !narrow || (hasAnyMeeting && !showDetailNarrow);
  const showDetailPane = !narrow || showDetailNarrow || !hasAnyMeeting;

  // The test capture lives with the other settings: it checks the setup, and
  // it is not something to press during a meeting by accident.
  const checkSection: ReactNode = (
    <section style={{ marginBottom: 16 }}>
      <h3 style={{ margin: "0 0 8px", fontSize: 15 }}>Check your setup</h3>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button
          type="button"
          className="kea-btn kea-btn--primary"
          onClick={() => void runTestCapture()}
          disabled={recordDisabled}
        >
          Run a 10-second test
        </button>
      </div>
      <p className="kea-muted" style={{ margin: "8px 0 0", fontSize: "0.8125rem" }}>
        The test is a real capture — it is saved to your meetings like any other.
      </p>
    </section>
  );

  return (
    <div>
      <header style={{ display: "flex", alignItems: "flex-start", gap: 12, flexWrap: "wrap" }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <h1 style={{ marginTop: 0 }}>Meetings</h1>
          <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
            Record a meeting, watch the transcript appear live, and get notes when you
            stop.
          </p>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
          <span className="kea-muted" style={{ fontSize: 13 }}>
            State:{" "}
            <strong style={{ color: "var(--text)" }}>
              {state === "idle" ? "Idle" : state === "recording" ? "Recording" : "Processing"}
            </strong>
          </span>
          {recording && <LevelMeter level={level} />}
          <button
            type="button"
            className="kea-btn kea-btn--primary"
            onClick={() => void onStart()}
            disabled={recordDisabled}
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
          <button
            ref={settingsButtonRef}
            type="button"
            className="kea-btn"
            aria-label="Meeting settings"
            title="Meeting settings"
            aria-haspopup="dialog"
            aria-expanded={settingsOpen}
            onClick={() => (settingsOpen ? closeSettings() : setSettingsOpen(true))}
          >
            ⚙
          </button>
        </div>
      </header>

      {meetingStatus && (
        <p className="kea-muted" style={{ margin: "0 0 16px" }}>
          {meetingStatus}
        </p>
      )}

      <MeetingSettingsPanel
        open={settingsOpen}
        onClose={closeSettings}
        anchorRef={settingsButtonRef}
        settings={settings}
        meetingSettings={meetingSettings}
        saveSettings={saveSettings}
        onSegmentDurationChange={onSegmentDurationChange}
        capability={capability}
        calendarPerm={calendarPerm}
        requestCalendar={requestCalendar}
        actionBusy={actionBusy}
        voiceEnablePending={voiceEnablePending}
        voiceModelsReady={voiceModelsReady}
        voiceDownloading={voiceDownloading}
        voiceStatus={voiceStatus}
        toggleVoiceSeparation={toggleVoiceSeparation}
        downloadVoiceModels={downloadVoiceModels}
        ai={ai}
        onNotionStatusChange={setNotionStatus}
        checkSection={checkSection}
      />

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

      <div className={`kea-meetings-grid${narrow ? " kea-meetings-grid--narrow" : ""}`}>
        {showListPane && (
          <MeetingList
            meetings={listRows}
            recording={pinnedRow}
            selectedId={selectedId}
            onSelect={onSelectRow}
          />
        )}

        {showDetailPane && (
          <section className="kea-card" style={{ minHeight: 200, minWidth: 0 }}>
            {paneMeeting ? (
              <MeetingPane
                key={paneMeeting.id}
                ref={paneRef}
                meeting={paneMeeting}
                detail={detail}
                live={live}
                liveSegments={segments}
                liveNotes={liveNotes}
                interimNotes={settings.interim_notes}
                generating={generating}
                loading={detailLoading}
                busy={busy}
                onBack={narrow ? () => setShowDetailNarrow(false) : undefined}
                onDelete={live ? undefined : onDelete}
                onRenameTitle={onRenameTitle}
                onRenameSpeaker={onRenameSpeaker}
                onSetActionItemStatus={onSetActionItemStatus}
                onAddActionItem={onAddActionItem}
                onCopyMarkdown={onCopyMarkdown}
                onSaveMarkdown={onSaveMarkdown}
                onExportNotion={notionReady ? onExportNotion : undefined}
                aiReady={aiReady}
                aiBlockedReason={aiBlockedReason}
                onOpenModels={onNavigate ? () => onNavigate("models") : undefined}
                onNotesChanged={applyDetail}
              />
            ) : !hasAnyMeeting ? (
              <div>
                <h2 style={{ marginTop: 0, fontSize: 18 }}>No meetings yet</h2>
                <p className="kea-muted" style={{ margin: "0 0 16px" }}>
                  Record one and its transcript appears here as it happens, with notes
                  when you stop.
                </p>
                <button
                  type="button"
                  className="kea-btn kea-btn--primary"
                  onClick={() => void onStart()}
                  disabled={recordDisabled}
                >
                  Record a meeting
                </button>
              </div>
            ) : selectedId && detailLoading ? (
              <LoadingBlock label="Loading meeting…" />
            ) : (
              <p className="kea-muted" style={{ margin: 0 }}>
                Select a meeting to view notes and transcript.
              </p>
            )}
          </section>
        )}
      </div>

      {listStatus && (
        <p style={{ marginTop: 12, fontSize: 13, color: "var(--danger)" }}>
          {listStatus}
        </p>
      )}
    </div>
  );
}
