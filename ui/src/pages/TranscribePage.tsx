import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  cancelFileTranscription,
  deleteTranscript,
  exportTranscript,
  getTranscript,
  listTranscripts,
  onTranscribeFileComplete,
  onTranscribeFileError,
  onTranscribeFileProgress,
  onTranscribeFileSegment,
  pickAudioFile,
  renderTranscriptSubtitles,
  transcribeFile,
  type SubtitleFormat,
  type TranscribeFileProgress,
  type TranscriptDetail,
  type TranscriptRow,
} from "../api";
import Banner from "../components/Banner";
import LoadingBlock from "../components/LoadingBlock";
import StatusPill from "../components/StatusPill";
import TranscriptPanel, { type TranscriptSegment } from "../components/TranscriptPanel";
import { toMessage } from "../lib/format";
import type { Navigate } from "../lib/nav";

type Props = { onNavigate?: Navigate };

/**
 * Extensions the drop zone accepts.
 *
 * Advisory, mirroring `kea_platform::audio::is_probably_decodable`: the
 * decoder probes the bytes, so this only stops the obviously-wrong drop (a
 * PDF) from spending a second in the decoder.
 */
const DROPPABLE = /\.(wav|wave|mp3|m4a|m4b|mp4|mov|aac|flac|ogg|oga|opus|caf|aiff|aif|aifc|mka|mkv|webm|alac|amr|3gp)$/i;

function formatOffset(ms: number): string {
  const total = Math.floor(ms / 1000);
  const min = Math.floor(total / 60);
  const sec = total % 60;
  return `${String(min).padStart(2, "0")}:${String(sec).padStart(2, "0")}`;
}

/** Live cues arrive one event at a time, so they carry no sequence of their own. */
type LiveCue = { start_ms: number; end_ms: number; text: string };

function toPanelSegments(cues: LiveCue[]): TranscriptSegment[] {
  return cues.map((cue, index) => ({
    meeting_id: "",
    sequence: index,
    start_offset_ms: cue.start_ms,
    end_offset_ms: cue.end_ms,
    text: cue.text,
  }));
}

export default function TranscribePage({ onNavigate }: Props) {
  const [loading, setLoading] = useState(true);
  const [transcripts, setTranscripts] = useState<TranscriptRow[]>([]);
  const [selected, setSelected] = useState<TranscriptDetail | null>(null);
  const [live, setLive] = useState<LiveCue[]>([]);
  const [progress, setProgress] = useState<TranscribeFileProgress | null>(null);
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // Read inside event handlers that are registered once, so it has to be a
  // ref rather than state — a captured `running` would always be its mount
  // value.
  const runningRef = useRef(false);

  const refresh = useCallback(async () => {
    try {
      setTranscripts(await listTranscripts());
    } catch (e) {
      setError(toMessage(e));
    }
  }, []);

  useEffect(() => {
    void refresh().finally(() => setLoading(false));
  }, [refresh]);

  const start = useCallback(
    async (path: string) => {
      if (runningRef.current) {
        setError("a file is already being transcribed");
        return;
      }
      runningRef.current = true;
      setRunning(true);
      setStopping(false);
      setError(null);
      setNotice(null);
      setLive([]);
      setProgress(null);
      setSelected(null);
      try {
        await transcribeFile(path);
      } catch (e) {
        setError(toMessage(e));
      } finally {
        runningRef.current = false;
        setRunning(false);
        setStopping(false);
        void refresh();
      }
    },
    [refresh],
  );

  useEffect(() => {
    const unlisten: Promise<() => void>[] = [
      onTranscribeFileProgress(setProgress),
      onTranscribeFileSegment((seg) =>
        setLive((cues) => [...cues, { start_ms: seg.start_ms, end_ms: seg.end_ms, text: seg.text }]),
      ),
      onTranscribeFileComplete(async ({ transcript_id, cancelled }) => {
        setNotice(cancelled ? "Stopped. The part that finished was kept." : "Done.");
        try {
          setSelected(await getTranscript(transcript_id));
        } catch (e) {
          setError(toMessage(e));
        }
      }),
      onTranscribeFileError(setError),
    ];
    return () => {
      unlisten.forEach((p) => void p.then((off) => off()));
    };
  }, []);

  // Tauri v2's native drag-drop is on by default (tauri.conf.json sets no
  // `dragDropEnabled`), and with it a plain React `onDrop` handler never
  // fires — the paths arrive on the webview's own event instead. A div with
  // `onDrop` here would look correct and do nothing.
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "over" || event.payload.type === "enter") {
        setDragging(true);
        return;
      }
      setDragging(false);
      if (event.payload.type !== "drop") return;
      const path = event.payload.paths.find((p) => DROPPABLE.test(p));
      if (!path) {
        setError("that file is not an audio or video recording");
        return;
      }
      void start(path);
    });
    return () => {
      void unlisten.then((off) => off());
    };
  }, [start]);

  const choose = async () => {
    try {
      const path = await pickAudioFile();
      if (path) await start(path);
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const stop = async () => {
    setStopping(true);
    try {
      await cancelFileTranscription();
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const doExport = async (format: SubtitleFormat) => {
    if (!selected) return;
    try {
      const path = await exportTranscript(selected.transcript.id, format);
      setNotice(`Saved ${path}`);
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const copySubtitles = async (format: SubtitleFormat) => {
    if (!selected) return;
    try {
      const body = await renderTranscriptSubtitles(selected.transcript.id, format);
      await navigator.clipboard.writeText(body);
      setNotice(`Copied ${format.toUpperCase()} to the clipboard.`);
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const open = async (id: string) => {
    try {
      setSelected(await getTranscript(id));
      setLive([]);
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const remove = async (id: string) => {
    try {
      await deleteTranscript(id);
      if (selected?.transcript.id === id) setSelected(null);
      await refresh();
    } catch (e) {
      setError(toMessage(e));
    }
  };

  if (loading) return <LoadingBlock label="Loading transcripts…" minHeight={200} />;

  const percent =
    progress && progress.audio_ms_total > 0
      ? Math.min(100, Math.round((progress.audio_ms_done / progress.audio_ms_total) * 100))
      : 0;

  const panelSegments: TranscriptSegment[] = selected
    ? selected.segments.map((seg) => ({
        meeting_id: seg.transcript_id,
        sequence: seg.sequence,
        start_offset_ms: seg.start_ms,
        end_offset_ms: seg.end_ms,
        text: seg.text,
        speaker: seg.speaker_key,
      }))
    : toPanelSegments(live);

  return (
    <div>
      <header style={{ marginBottom: 16 }}>
        <h2 style={{ margin: "0 0 4px" }}>Transcribe a file</h2>
        <p className="kea-muted" style={{ margin: 0 }}>
          Drop an audio or video recording here to get a timed transcript you can export as
          subtitles.
        </p>
      </header>

      {error && (
        <Banner
          variant="error"
          action={
            <button type="button" className="kea-btn" onClick={() => setError(null)}>
              Dismiss
            </button>
          }
        >
          {error}
        </Banner>
      )}

      <section
        className="kea-card"
        aria-label="Drop zone"
        style={{
          marginBottom: 16,
          borderStyle: "dashed",
          borderWidth: 2,
          borderColor: dragging ? "var(--accent)" : "var(--border)",
          textAlign: "center",
          padding: 24,
        }}
      >
        <p style={{ margin: "0 0 12px" }}>
          {dragging ? "Drop it to start" : "Drag a recording here"}
        </p>
        <button type="button" className="kea-btn" onClick={choose} disabled={running}>
          Choose file…
        </button>
      </section>

      {running && (
        <section className="kea-card" style={{ marginBottom: 16 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
            <progress value={percent} max={100} style={{ flex: 1 }} />
            <span className="kea-muted">
              {progress
                ? `${formatOffset(progress.audio_ms_done)} of ${formatOffset(
                    progress.audio_ms_total,
                  )} · chunk ${progress.chunk_index + 1}/${progress.chunk_count}`
                : "Decoding…"}
            </span>
            <button type="button" className="kea-btn" onClick={stop} disabled={stopping}>
              {/* Honest about the latency: a decode already running cannot be
                  interrupted, so a stop lands at the end of the chunk. */}
              {stopping ? "Stopping…" : "Stop"}
            </button>
          </div>
        </section>
      )}

      <section style={{ marginBottom: 16 }}>
        <h3 style={{ margin: "0 0 8px" }}>Transcript</h3>
        <TranscriptPanel
          segments={panelSegments}
          live={running}
          emptyMessage="Nothing transcribed yet."
        />
        {selected && (
          <div style={{ display: "flex", gap: 8, marginTop: 8, flexWrap: "wrap" }}>
            <button type="button" className="kea-btn" onClick={() => doExport("srt")}>
              Export SRT
            </button>
            <button type="button" className="kea-btn" onClick={() => doExport("vtt")}>
              Export VTT
            </button>
            <button type="button" className="kea-btn" onClick={() => copySubtitles("srt")}>
              Copy SRT
            </button>
          </div>
        )}
      </section>

      <section>
        <h3 style={{ margin: "0 0 8px" }}>Recent files</h3>
        {transcripts.length === 0 ? (
          <p className="kea-muted" style={{ margin: 0 }}>
            No files transcribed yet.
          </p>
        ) : (
          <ul style={{ margin: 0, padding: 0, listStyle: "none" }}>
            {transcripts.map((row) => (
              <li
                key={row.id}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  padding: "6px 0",
                  borderBottom: "1px solid var(--border)",
                }}
              >
                <button
                  type="button"
                  className="kea-btn"
                  style={{ flex: 1, textAlign: "left" }}
                  onClick={() => open(row.id)}
                >
                  {row.source_filename}
                </button>
                <span className="kea-muted">{row.status}</span>
                <button
                  type="button"
                  className="kea-btn"
                  onClick={() => remove(row.id)}
                  style={{ color: "var(--danger)", borderColor: "var(--danger)" }}
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      {onNavigate && (
        <p className="kea-muted" style={{ marginTop: 16 }}>
          Uses the speech-to-text engine from{" "}
          <button type="button" className="kea-link" onClick={() => onNavigate("models")}>
            Models
          </button>
          .
        </p>
      )}

      <StatusPill message={notice} variant={notice ? "progress" : null} />
    </div>
  );
}
