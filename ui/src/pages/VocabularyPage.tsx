import { useEffect, useState, type FormEvent } from "react";
import {
  deleteVocabularyEntry,
  listVocabulary,
  previewVocabulary,
  upsertVocabularyEntry,
  type VocabularyEntry,
} from "../api";
import LoadingBlock from "../components/LoadingBlock";
import Toggle from "../components/Toggle";
import { toMessage } from "../lib/format";

/**
 * Long enough that a fast typist sends one request per phrase rather than one
 * per keystroke, short enough that the result still reads as live feedback —
 * this box is the only place the replacement rules are visible, so a lag that
 * makes it feel batched costs more than the saved round-trips.
 */
const PREVIEW_DEBOUNCE_MS = 200;

/**
 * Ids follow the preset convention (`preset-${Date.now()}`), with the index
 * mixed in because a bulk import mints a whole block inside one millisecond
 * and identical ids would upsert over each other.
 */
function newEntry(term: string, soundsLike: string | null, index: number): VocabularyEntry {
  return {
    id: `vocab-${Date.now()}-${index}`,
    term,
    sounds_like: soundsLike,
    enabled: true,
    created_at: new Date().toISOString(),
  };
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

export default function VocabularyPage() {
  const [entries, setEntries] = useState<VocabularyEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [term, setTerm] = useState("");
  const [soundsLike, setSoundsLike] = useState("");

  const [importText, setImportText] = useState("");
  const [importStatus, setImportStatus] = useState<string | null>(null);

  const [sample, setSample] = useState("");
  const [preview, setPreview] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);

  useEffect(() => {
    listVocabulary()
      .then(setEntries)
      .catch((e) => setError(toMessage(e)))
      .finally(() => setLoading(false));
  }, []);

  // `entries` is a dependency because the backend previews against whatever is
  // stored: adding a term or flipping a toggle changes the answer for text the
  // user has already typed, and a stale preview is worse than none.
  useEffect(() => {
    if (!sample.trim()) {
      setPreview(null);
      setPreviewError(null);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      previewVocabulary(sample)
        .then((next) => {
          if (cancelled) return;
          setPreview(next);
          setPreviewError(null);
        })
        .catch((e) => {
          if (cancelled) return;
          setPreview(null);
          setPreviewError(toMessage(e));
        });
    }, PREVIEW_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [sample, entries]);

  /**
   * The stored terms, lower-cased. The unique index is `COLLATE NOCASE`, so a
   * case variant of something already stored is a constraint violation, not a
   * second row — checking here turns a raw SQL error into a sentence.
   */
  const knownTerms = new Set(entries.map((entry) => entry.term.toLowerCase()));

  const onAdd = async (event: FormEvent) => {
    event.preventDefault();
    const name = term.trim();
    if (!name) return;
    if (knownTerms.has(name.toLowerCase())) {
      setError(`"${name}" is already in your vocabulary.`);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await upsertVocabularyEntry(newEntry(name, soundsLike.trim() || null, 0));
      setEntries(await listVocabulary());
      setTerm("");
      setSoundsLike("");
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const onDelete = async (entry: VocabularyEntry) => {
    setBusy(true);
    setError(null);
    try {
      await deleteVocabularyEntry(entry.id);
      setEntries((prev) => prev.filter((row) => row.id !== entry.id));
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  // Optimistic, with the previous list put back on failure: the row must never
  // keep showing a state the backend refused.
  const onToggle = async (entry: VocabularyEntry, enabled: boolean) => {
    const previous = entries;
    setEntries((prev) =>
      prev.map((row) => (row.id === entry.id ? { ...row, enabled } : row)),
    );
    setError(null);
    try {
      await upsertVocabularyEntry({ ...entry, enabled });
    } catch (e) {
      setEntries(previous);
      setError(toMessage(e));
    }
  };

  const onImport = async () => {
    const lines = importText
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean);
    if (lines.length === 0) return;

    // `seen` starts from what is stored and grows as the block is walked, so a
    // term repeated inside the pasted text counts as a duplicate too.
    const seen = new Set(knownTerms);
    const fresh: VocabularyEntry[] = [];
    let skipped = 0;
    for (const line of lines) {
      const key = line.toLowerCase();
      if (seen.has(key)) {
        skipped += 1;
        continue;
      }
      seen.add(key);
      fresh.push(newEntry(line, null, fresh.length));
    }

    setBusy(true);
    setError(null);
    setImportStatus(null);
    try {
      // Sequential on purpose: if one row is rejected the ones before it are
      // already stored, and the reload below shows exactly how far it got.
      for (const entry of fresh) {
        await upsertVocabularyEntry(entry);
      }
      setImportText("");
      setImportStatus(
        `Added ${plural(fresh.length, "term")}, skipped ${plural(skipped, "duplicate")}.`,
      );
    } catch (e) {
      setError(toMessage(e));
    } finally {
      try {
        setEntries(await listVocabulary());
      } catch (e) {
        setError(toMessage(e));
      }
      setBusy(false);
    }
  };

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>Vocabulary</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
          Names, acronyms and product terms KEA keeps getting wrong — it hints the
          speech model with them and fixes near-misses in every transcript.
        </p>
      </header>

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Your terms</h2>
        {loading ? (
          <LoadingBlock label="Loading vocabulary…" minHeight={44} />
        ) : entries.length === 0 ? (
          <div className="kea-card">
            <p className="kea-muted" style={{ margin: 0 }}>
              No terms yet. Add one below and KEA will spell it your way wherever it
              hears it.
            </p>
          </div>
        ) : (
          <div className="kea-table-wrap">
            <table className="kea-table">
              <caption className="kea-visually-hidden">Vocabulary terms</caption>
              <thead>
                <tr>
                  <th scope="col">Term</th>
                  <th scope="col">Sounds like</th>
                  <th scope="col">Enabled</th>
                  <th scope="col" className="kea-table__actions">
                    <span className="kea-visually-hidden">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {entries.map((entry) => (
                  <tr key={entry.id}>
                    <td>{entry.term}</td>
                    <td>{entry.sounds_like ?? "—"}</td>
                    <td>
                      <Toggle
                        label={`Use ${entry.term}`}
                        checked={entry.enabled}
                        disabled={busy}
                        onChange={(next) => void onToggle(entry, next)}
                      />
                    </td>
                    <td className="kea-table__actions">
                      <button
                        type="button"
                        className="kea-btn"
                        aria-label={`Delete ${entry.term}`}
                        disabled={busy}
                        onClick={() => void onDelete(entry)}
                      >
                        Delete
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {error && (
          <p role="alert" style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
            {error}
          </p>
        )}

        <form className="kea-toolbar" style={{ marginTop: 16 }} onSubmit={(e) => void onAdd(e)}>
          <label>
            <span className="kea-label">Term</span>
            <input
              className="kea-input"
              aria-label="Term"
              value={term}
              onChange={(e) => setTerm(e.target.value)}
              placeholder="KittyClaw"
            />
          </label>
          <label className="kea-toolbar__grow">
            <span className="kea-label">Sounds like (optional)</span>
            <input
              className="kea-input"
              style={{ width: "100%" }}
              aria-label="Sounds like"
              value={soundsLike}
              onChange={(e) => setSoundsLike(e.target.value)}
              placeholder="kitty claw, kitty paw"
            />
          </label>
          <button type="submit" className="kea-btn kea-btn--primary" disabled={busy || !term.trim()}>
            Add term
          </button>
        </form>
      </section>

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Import a list</h2>
        <div className="kea-card">
          <label style={{ display: "block", marginBottom: 12 }}>
            <span className="kea-label">Terms to import — one per line</span>
            <textarea
              className="kea-input"
              aria-label="Terms to import"
              value={importText}
              onChange={(e) => setImportText(e.target.value)}
              rows={4}
              style={{ width: "100%", maxWidth: 520, resize: "vertical" }}
              placeholder={"KittyClaw\nKEA\nParakeet"}
            />
          </label>
          <button
            type="button"
            className="kea-btn"
            disabled={busy || !importText.trim()}
            onClick={() => void onImport()}
          >
            Import terms
          </button>
          {importStatus && (
            <p className="kea-muted" role="status" style={{ marginTop: 12, marginBottom: 0 }}>
              {importStatus}
            </p>
          )}
        </div>
      </section>

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Test it</h2>
        <div className="kea-card">
          <label style={{ display: "block", marginBottom: 12 }}>
            <span className="kea-label">Test sentence</span>
            <textarea
              className="kea-input"
              aria-label="Test sentence"
              value={sample}
              onChange={(e) => setSample(e.target.value)}
              rows={2}
              style={{ width: "100%", maxWidth: 520, resize: "vertical" }}
              placeholder="Type what KEA mis-hears, e.g. “open kitty claw”"
            />
          </label>
          {/* Polite rather than assertive: the result changes on every pause in
              typing, and an assertive region would interrupt the typing itself. */}
          <div aria-live="polite">
            {previewError ? (
              <p style={{ margin: 0, fontSize: "0.8125rem", color: "var(--danger)" }}>
                {previewError}
              </p>
            ) : preview === null ? (
              <p className="kea-muted" style={{ margin: 0 }}>
                Type a sentence to see what your vocabulary would do to it.
              </p>
            ) : preview === sample ? (
              <p className="kea-muted" style={{ margin: 0 }}>
                No changes — nothing in your vocabulary matched this text.
              </p>
            ) : (
              <div>
                <span className="kea-label">With your vocabulary</span>
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
                  {preview}
                </p>
              </div>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}
