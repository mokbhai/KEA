import { useEffect, useState, type FormEvent } from "react";
import {
  captureAppContext,
  deleteAppProfile,
  getSetting,
  listAppProfiles,
  listLlmEngines,
  listPresets,
  listProviders,
  setSetting,
  upsertAppProfile,
  CAPTURE_URL_SETTING,
  CAPTURE_WINDOW_TITLE_SETTING,
  INSERTION_MODES,
  REWRITE_MODES,
  type AppProfile,
  type Provider,
  type RewritePreset,
} from "../api";
import LoadingBlock from "../components/LoadingBlock";
import { Row, RowGroup } from "../components/SettingsRow";
import Toggle from "../components/Toggle";
import { useOptimisticSetting } from "../hooks/useOptimisticSetting";
import { enginesFor } from "../lib/engines";
import { toMessage } from "../lib/format";

/**
 * The `<select>` value standing for "inherit the global setting", which is
 * stored as null. The empty string, because no mode, preset id or engine id
 * can be empty.
 */
const INHERIT = "";

/**
 * The three states of `post_process`, spelled out.
 *
 * Inherit is not the same as off, and a checkbox would have to pick one of
 * them to mean the other — which is how every app that never opted in would
 * quietly lose its AI clean-up.
 */
const CLEANUP_CHOICES: { value: string; label: string; stored: boolean | null }[] = [
  { value: "inherit", label: "Inherit — whatever Dictation is set to", stored: null },
  { value: "on", label: "Always clean up with AI", stored: true },
  { value: "off", label: "Never — insert exactly what I said", stored: false },
];

const cleanupValue = (stored: boolean | null) =>
  stored === null ? "inherit" : stored ? "on" : "off";

const cleanupStored = (value: string) =>
  CLEANUP_CHOICES.find((c) => c.value === value)?.stored ?? null;

/**
 * How specific a rule's match keys are, mirroring `AppProfile::specificity`:
 * app + web address beats app, beats web address, beats "any app".
 */
const specificity = (p: AppProfile) =>
  (p.match_bundle_id ? 2 : 0) + (p.match_url_glob ? 1 : 0);

/**
 * The order the backend resolves in — specificity, then priority, then the
 * lowest id (crates/core/src/app_context/resolve.rs). The list is shown in
 * this order because "the first rule that matches wins" is only true if the
 * screen agrees with the resolver about what "first" means.
 */
const byPrecedence = (a: AppProfile, b: AppProfile) =>
  specificity(b) - specificity(a) ||
  b.priority - a.priority ||
  (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);

/** What this rule matches, in the order the resolver reads it. */
function matchSummary(p: AppProfile): string {
  if (p.match_bundle_id && p.match_url_glob) {
    return `${p.match_bundle_id} · ${p.match_url_glob}`;
  }
  if (p.match_bundle_id) return p.match_bundle_id;
  if (p.match_url_glob) return `any app · ${p.match_url_glob}`;
  return "Any app";
}

/** What this rule changes, or what to say when it changes nothing. */
function effectSummary(p: AppProfile, presets: RewritePreset[]): string {
  const parts: string[] = [];
  const preset = presets.find((r) => r.id === p.preset_id);
  if (preset) parts.push(preset.name);
  else if (p.rewrite_mode) {
    parts.push(REWRITE_MODES.find((m) => m.value === p.rewrite_mode)?.label ?? p.rewrite_mode);
  }
  if (p.post_process !== null) parts.push(p.post_process ? "AI clean-up" : "no AI clean-up");
  if (p.insertion_mode) {
    parts.push(
      INSERTION_MODES.find((m) => m.value === p.insertion_mode)?.label ?? p.insertion_mode,
    );
  }
  if (p.llm_engine_id) parts.push(p.llm_model ?? p.llm_engine_id);
  return parts.length > 0 ? parts.join(" · ") : "Nothing yet";
}

/**
 * A blank rule. Priority starts above every existing one so a rule just
 * written wins over an equally specific older one — which is what someone who
 * has just written it expects.
 */
function newProfile(existing: AppProfile[]): AppProfile {
  return {
    id: `profile-${Date.now()}`,
    name: "",
    enabled: true,
    priority: existing.reduce((max, p) => Math.max(max, p.priority), 0) + 1,
    match_bundle_id: null,
    match_url_glob: null,
    rewrite_mode: null,
    preset_id: null,
    llm_engine_id: null,
    llm_model: null,
    llm_provider_ref: null,
    post_process: null,
    insertion_mode: null,
    // Stamped by the database on first insert; a round-tripped row keeps the
    // timestamp it already has.
    created_at: "",
  };
}

/** Trimmed, or null — an empty match key means "any", never "". */
const orNull = (value: string) => (value.trim() === "" ? null : value.trim());

type CaptureSettings = { windowTitle: boolean; url: boolean };

export default function ProfilesPage() {
  const [profiles, setProfiles] = useState<AppProfile[] | null>(null);
  const [presets, setPresets] = useState<RewritePreset[]>([]);
  const [llmEngines, setLlmEngines] = useState<string[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [draft, setDraft] = useState<AppProfile | null>(null);
  const [capturing, setCapturing] = useState(false);
  const [captureNote, setCaptureNote] = useState<string | null>(null);

  const capture = useOptimisticSetting<CaptureSettings>({
    initial: { windowTitle: false, url: false },
    // Both keys are written on every change. The hook hands `persist` the
    // whole object rather than the patch, and re-writing the unchanged key is
    // cheaper than a second hook to keep them apart.
    persist: async (next) => {
      await setSetting(CAPTURE_WINDOW_TITLE_SETTING, String(next.windowTitle));
      await setSetting(CAPTURE_URL_SETTING, String(next.url));
    },
  });
  const { setValue: setCaptureValue } = capture;

  useEffect(() => {
    listAppProfiles()
      .then(setProfiles)
      .catch((e) => {
        setProfiles([]);
        setError(toMessage(e));
      });
    // The editor's dropdowns degrade to "inherit only" rather than taking the
    // page down with them.
    listPresets()
      .then(setPresets)
      .catch(() => setPresets([]));
    listLlmEngines()
      .then((infos) => {
        const known = new Set<string>(enginesFor("llm").map((e) => e.id));
        setLlmEngines(infos.map((e) => e.id).filter((id) => known.has(id)));
      })
      .catch(() => setLlmEngines([]));
    listProviders()
      .then(setProviders)
      .catch(() => setProviders([]));
    Promise.all([
      getSetting(CAPTURE_WINDOW_TITLE_SETTING),
      getSetting(CAPTURE_URL_SETTING),
    ])
      .then(([title, url]) =>
        setCaptureValue({ windowTitle: title === "true", url: url === "true" }),
      )
      .catch(() => {}); /* both default to off, which is what is already shown */
  }, [setCaptureValue]);

  const ordered = [...(profiles ?? [])].sort(byPrecedence);

  const reload = async () => {
    setProfiles(await listAppProfiles());
  };

  const onSave = async (event: FormEvent) => {
    event.preventDefault();
    if (!draft || !draft.name.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await upsertAppProfile({ ...draft, name: draft.name.trim() });
      await reload();
      setDraft(null);
      setCaptureNote(null);
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const onDelete = async (profile: AppProfile) => {
    setBusy(true);
    setError(null);
    try {
      await deleteAppProfile(profile.id);
      if (draft?.id === profile.id) setDraft(null);
      await reload();
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  // Optimistic, with the list put back on failure: a row must never keep
  // showing a state the backend refused.
  const onToggle = async (profile: AppProfile, enabled: boolean) => {
    const previous = profiles ?? [];
    setProfiles(previous.map((p) => (p.id === profile.id ? { ...p, enabled } : p)));
    setError(null);
    try {
      await upsertAppProfile({ ...profile, enabled });
    } catch (e) {
      setProfiles(previous);
      setError(toMessage(e));
    }
  };

  /**
   * Swaps a rule with its neighbour and renumbers every priority to match the
   * list. Renumbering the whole list rather than trading two values keeps the
   * numbers distinct, so the next move is not a no-op between two rules that
   * happened to share a priority.
   */
  const onMove = async (index: number, delta: number) => {
    const next = [...ordered];
    const [moved] = next.splice(index, 1);
    next.splice(index + delta, 0, moved);
    const renumbered = next.map((p, i) => ({ ...p, priority: next.length - i }));
    setBusy(true);
    setError(null);
    try {
      // Sequential: if one write is refused the ones before it are already
      // stored, and the reload below shows exactly how far it got.
      for (const p of renumbered) {
        if (p.priority !== ordered.find((o) => o.id === p.id)?.priority) {
          await upsertAppProfile(p);
        }
      }
    } catch (e) {
      setError(toMessage(e));
    } finally {
      try {
        await reload();
      } catch (e) {
        setError(toMessage(e));
      }
      setBusy(false);
    }
  };

  const onCapture = async () => {
    setCapturing(true);
    setCaptureNote(null);
    setError(null);
    try {
      const ctx = await captureAppContext();
      if (!ctx?.bundle_id) {
        setCaptureNote(
          "KEA was still the app in front, so there was nothing to identify. Click again, then switch to the app you want.",
        );
        return;
      }
      setDraft((d) => (d ? { ...d, match_bundle_id: ctx.bundle_id } : d));
      const seen = [`Captured ${ctx.app_name ?? "an app"} — ${ctx.bundle_id}`];
      if (ctx.window_title) seen.push(`window “${ctx.window_title}”`);
      if (ctx.url) seen.push(`page ${ctx.url}`);
      setCaptureNote(seen.join(" · "));
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setCapturing(false);
    }
  };

  const editField = (patch: Partial<AppProfile>) =>
    setDraft((d) => (d ? { ...d, ...patch } : d));

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>App profiles</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
          Rules that change what KEA does depending on the app you are typing into —
          Friendly in Slack, raw text with no AI clean-up in Terminal.
        </p>
      </header>

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 4px" }}>Your rules</h2>
        <p className="kea-muted" style={{ margin: "0 0 12px", fontSize: "0.8125rem" }}>
          KEA uses the first rule here that matches. A rule naming both an app and a
          web address always beats a broader one, whatever the order; the arrows
          decide between rules that match equally specifically.
        </p>

        {profiles === null ? (
          <LoadingBlock label="Loading profiles…" minHeight={44} />
        ) : ordered.length === 0 ? (
          <div className="kea-card">
            <p className="kea-muted" style={{ margin: 0 }}>
              No rules yet — add one to give an app its own style, its own AI, or no AI
              at all.
            </p>
          </div>
        ) : (
          <div className="kea-table-wrap">
            <table className="kea-table">
              <caption className="kea-visually-hidden">
                App profiles, in the order KEA checks them
              </caption>
              <thead>
                <tr>
                  <th scope="col">Order</th>
                  <th scope="col">Rule</th>
                  <th scope="col">Matches</th>
                  <th scope="col">Changes</th>
                  <th scope="col">On</th>
                  <th scope="col" className="kea-table__actions">
                    <span className="kea-visually-hidden">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {ordered.map((profile, index) => {
                  // Priority only orders rules of equal specificity, so an
                  // arrow that would trade places across that line is dead:
                  // the sort would put both rows straight back.
                  const sameBand = (other?: AppProfile) =>
                    !!other && specificity(other) === specificity(profile);
                  return (
                    <tr key={profile.id}>
                      <td>
                        <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
                          <span className="kea-muted">{index + 1}</span>
                          <button
                            type="button"
                            className="kea-btn"
                            aria-label={`Move ${profile.name} up`}
                            disabled={busy || !sameBand(ordered[index - 1])}
                            onClick={() => void onMove(index, -1)}
                          >
                            ↑
                          </button>
                          <button
                            type="button"
                            className="kea-btn"
                            aria-label={`Move ${profile.name} down`}
                            disabled={busy || !sameBand(ordered[index + 1])}
                            onClick={() => void onMove(index, 1)}
                          >
                            ↓
                          </button>
                        </div>
                      </td>
                      <td>{profile.name}</td>
                      <td>{matchSummary(profile)}</td>
                      <td>{effectSummary(profile, presets)}</td>
                      <td>
                        <Toggle
                          label={`Use ${profile.name}`}
                          checked={profile.enabled}
                          disabled={busy}
                          onChange={(next) => void onToggle(profile, next)}
                        />
                      </td>
                      <td className="kea-table__actions">
                        <button
                          type="button"
                          className="kea-btn"
                          aria-label={`Edit ${profile.name}`}
                          disabled={busy}
                          onClick={() => {
                            setCaptureNote(null);
                            setDraft(profile);
                          }}
                        >
                          Edit
                        </button>
                        <button
                          type="button"
                          className="kea-btn"
                          aria-label={`Delete ${profile.name}`}
                          disabled={busy}
                          onClick={() => void onDelete(profile)}
                        >
                          Delete
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}

        {error && (
          <p
            role="alert"
            style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}
          >
            {error}
          </p>
        )}

        {!draft && (
          <button
            type="button"
            className="kea-btn kea-btn--primary"
            style={{ marginTop: 16 }}
            disabled={busy || profiles === null}
            onClick={() => {
              setCaptureNote(null);
              setDraft(newProfile(profiles ?? []));
            }}
          >
            New rule
          </button>
        )}
      </section>

      {draft && (
        <section style={{ marginBottom: 24 }}>
          <h2 style={{ margin: "0 0 12px" }}>
            {profiles?.some((p) => p.id === draft.id) ? "Edit rule" : "New rule"}
          </h2>
          <form className="kea-card" onSubmit={(e) => void onSave(e)}>
            <RowGroup aria-label="Rule">
              <Row label="Name" hint="Just for you — “Slack”, “Terminal”, “Work email”.">
                <input
                  className="kea-input"
                  aria-label="Rule name"
                  value={draft.name}
                  onChange={(e) => editField({ name: e.target.value })}
                  placeholder="Slack"
                />
              </Row>
              <Row
                label="App"
                hint="The app's bundle id, matched whatever its capitalisation. Leave it empty to match every app."
              >
                <input
                  className="kea-input"
                  aria-label="Bundle id"
                  value={draft.match_bundle_id ?? ""}
                  onChange={(e) => editField({ match_bundle_id: orNull(e.target.value) })}
                  placeholder="com.tinyspeck.slackmacgap"
                />
              </Row>
              <Row
                label="Find the app"
                hint="Clicking here puts KEA itself in front, so it waits a few seconds: click, switch to the app you want, and it reads whichever app is frontmost then."
              >
                <button
                  type="button"
                  className="kea-btn"
                  disabled={capturing || busy}
                  onClick={() => void onCapture()}
                >
                  {capturing ? "Watching…" : "Use the app I switch to"}
                </button>
              </Row>
              <Row
                label="Web address"
                hint="A * pattern over the page address, e.g. *.slack.com/*. It is only ever matched while you type — KEA never stores the address on the rule. Needs “Read the web address” below, or the rule never matches at all."
              >
                <input
                  className="kea-input"
                  aria-label="Web address pattern"
                  value={draft.match_url_glob ?? ""}
                  onChange={(e) => editField({ match_url_glob: orNull(e.target.value) })}
                  placeholder="*.slack.com/*"
                />
              </Row>
              <Row label="Style" hint="The rewrite style to use in this app.">
                <select
                  className="kea-select"
                  aria-label="Rewrite style"
                  value={draft.rewrite_mode ?? INHERIT}
                  onChange={(e) => editField({ rewrite_mode: orNull(e.target.value) })}
                >
                  <option value={INHERIT}>Inherit — whatever Rewrite is set to</option>
                  {REWRITE_MODES.map((m) => (
                    <option key={m.value} value={m.value}>
                      {m.label}
                    </option>
                  ))}
                  {/* A mode saved by a newer build still has to show as the
                      selection, or this dropdown would silently re-target it. */}
                  {draft.rewrite_mode &&
                    !REWRITE_MODES.some((m) => m.value === draft.rewrite_mode) && (
                      <option value={draft.rewrite_mode}>{draft.rewrite_mode}</option>
                    )}
                </select>
              </Row>
              <Row label="Preset" hint="A saved instruction, used instead of the style.">
                <select
                  className="kea-select"
                  aria-label="Rewrite preset"
                  value={draft.preset_id ?? INHERIT}
                  onChange={(e) => editField({ preset_id: orNull(e.target.value) })}
                >
                  <option value={INHERIT}>Inherit — whatever Rewrite is set to</option>
                  {presets.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </Row>
              <Row
                label="AI clean-up"
                hint="Whether dictation into this app goes through the clean-up pass. Inherit is not the same as off."
              >
                <select
                  className="kea-select"
                  aria-label="AI clean-up"
                  value={cleanupValue(draft.post_process)}
                  onChange={(e) =>
                    editField({ post_process: cleanupStored(e.target.value) })
                  }
                >
                  {CLEANUP_CHOICES.map((c) => (
                    <option key={c.value} value={c.value}>
                      {c.label}
                    </option>
                  ))}
                </select>
              </Row>
              <Row
                label="Putting the text back"
                hint="Some apps take typed text badly and paste cleanly, or the other way round."
              >
                <select
                  className="kea-select"
                  aria-label="Putting the text back"
                  value={draft.insertion_mode ?? INHERIT}
                  onChange={(e) => editField({ insertion_mode: orNull(e.target.value) })}
                >
                  <option value={INHERIT}>Inherit — whatever KEA does elsewhere</option>
                  {INSERTION_MODES.map((m) => (
                    <option key={m.value} value={m.value}>
                      {m.label}
                    </option>
                  ))}
                </select>
              </Row>
            </RowGroup>

            <details className="kea-advanced">
              <summary>Advanced</summary>
              <div className="kea-advanced__body">
                <p className="kea-muted" style={{ margin: 0, fontSize: "0.8125rem" }}>
                  A different AI for this app only. Leave the engine on Inherit and the
                  rest is ignored — a model with no engine names nothing KEA can resolve.
                </p>
                <label>
                  <span className="kea-label">AI engine</span>
                  <select
                    className="kea-select"
                    aria-label="AI engine"
                    value={draft.llm_engine_id ?? INHERIT}
                    onChange={(e) => editField({ llm_engine_id: orNull(e.target.value) })}
                  >
                    <option value={INHERIT}>Inherit — the Rewrite AI</option>
                    {llmEngines.map((id) => (
                      <option key={id} value={id}>
                        {id}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  <span className="kea-label">Model</span>
                  <input
                    className="kea-input"
                    aria-label="Model"
                    value={draft.llm_model ?? ""}
                    onChange={(e) => editField({ llm_model: orNull(e.target.value) })}
                    placeholder="gpt-4o-mini"
                  />
                </label>
                <label>
                  <span className="kea-label">Provider</span>
                  <select
                    className="kea-select"
                    aria-label="Provider"
                    value={draft.llm_provider_ref ?? INHERIT}
                    onChange={(e) =>
                      editField({ llm_provider_ref: orNull(e.target.value) })
                    }
                  >
                    <option value={INHERIT}>Inherit — the engine's own</option>
                    {providers.map((p) => (
                      <option key={p.provider_ref} value={p.provider_ref}>
                        {p.name}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
            </details>

            {captureNote && (
              <p role="status" className="kea-muted" style={{ margin: "12px 0 0" }}>
                {captureNote}
              </p>
            )}
            {capturing && (
              <p role="status" className="kea-muted" style={{ margin: "12px 0 0" }}>
                Switch to the app you want now — KEA reads whatever is in front in a
                moment.
              </p>
            )}

            <div style={{ display: "flex", gap: 8, marginTop: 16, flexWrap: "wrap" }}>
              <button
                type="submit"
                className="kea-btn kea-btn--primary"
                disabled={busy || !draft.name.trim()}
              >
                Save rule
              </button>
              <button
                type="button"
                className="kea-btn"
                disabled={busy}
                onClick={() => {
                  setDraft(null);
                  setCaptureNote(null);
                }}
              >
                Cancel
              </button>
            </div>
          </form>
        </section>
      )}

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>What KEA may look at</h2>
        <RowGroup aria-label="What KEA may look at">
          <Row
            label="Read the window title"
            hint="The title of the window you are typing into. Off by default; it is shown back to you here and in the logs, and never stored on a rule."
          >
            {capture.savedKey === "window_title" && (
              <span className="kea-saved">Saved ✓</span>
            )}
            <Toggle
              label="Read the window title"
              checked={capture.value.windowTitle}
              disabled={capture.busy}
              onChange={(next) =>
                void capture.save({ windowTitle: next }, "window_title")
              }
            />
          </Row>
          <Row
            label="Read the web address"
            hint="Reads the page address of the window you are typing into through Accessibility. It works in some browsers and not others, it is never stored on a rule, and it is used only to match a rule while you type."
          >
            {capture.savedKey === "url" && <span className="kea-saved">Saved ✓</span>}
            <Toggle
              label="Read the web address"
              checked={capture.value.url}
              disabled={capture.busy}
              onChange={(next) => void capture.save({ url: next }, "url")}
            />
          </Row>
        </RowGroup>
        {capture.error && (
          <p
            role="alert"
            style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}
          >
            {capture.error}
          </p>
        )}
      </section>
    </div>
  );
}
