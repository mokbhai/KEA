import { useEffect, useState } from "react";
import { listProviderModels } from "../api";
import type { Capability, CapabilityOption } from "../lib/capabilityDefaults";
import { toMessage } from "../lib/format";

/** Mounted only for an expanded provider. Stale requests cannot update a new provider. */
export default function ProviderModels({
  option,
  capability,
  selected,
  busy,
  onPick,
}: {
  option: CapabilityOption;
  capability: Capability;
  selected: string | null;
  busy: boolean;
  onPick: (option: CapabilityOption) => Promise<void>;
}) {
  const [saving, setSaving] = useState(false);
  const disabled = busy || saving;
  const [models, setModels] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState("");
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    listProviderModels(option.providerRef!, capability)
      .then((list) => {
        if (!cancelled) setModels(list);
      })
      .catch((e) => {
        if (!cancelled) {
          setError(toMessage(e));
          setModels([]);
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [option.providerRef, capability, attempt]);

  const pick = async (model: string) => {
    if (disabled) return;
    setSaving(true);
    try {
      await onPick({
        ...option,
        id: `${option.id}:${model}`,
        label: `${option.label} · ${model}`,
        model,
      });
    } finally {
      setSaving(false);
    }
  };
  const filtered = models.filter((id) => id.toLowerCase().includes(query.toLowerCase()));
  return (
    <div className="kea-picker__models" aria-label={`${option.label} models`}>
      {loading ? (
        <p role="status">Loading models…</p>
      ) : (
        <>
          {error && <p role="alert">{error}</p>}
          {!error && models.length === 0 && <p>No models returned for this capability.</p>}
          {(error || models.length === 0) && (
            <button type="button" className="kea-btn" onClick={() => setAttempt((n) => n + 1)}>
              Retry
            </button>
          )}
          {models.length > 0 && (
            <>
              <input
                className="kea-input"
                aria-label="Search models"
                placeholder="Search models"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
              <ul className="kea-picker__model-list" aria-label="Models">
                {filtered.map((model) => (
                  <li key={model}>
                    <button
                      type="button"
                      className="kea-picker__option"
                      disabled={disabled}
                      aria-pressed={selected === model}
                      onClick={() => void pick(model)}
                    >
                      <span>{model}</span>
                      {selected === model && <span aria-hidden="true">✓</span>}
                    </button>
                  </li>
                ))}
              </ul>
              {filtered.length === 0 && <p>No models match your search.</p>}
            </>
          )}
        </>
      )}
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (draft.trim() && !disabled) void pick(draft.trim());
        }}
      >
        <label>
          <span className="kea-label">Type a model id</span>
          <input
            className="kea-input"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="Custom model id"
          />
        </label>
        <button type="submit" className="kea-btn" disabled={disabled || !draft.trim()}>
          Use model
        </button>
      </form>
    </div>
  );
}
