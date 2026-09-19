import { useCallback, useEffect, useState } from "react";

import {
  clearUsage,
  deleteLlmRate,
  getUsageReport,
  listLlmRates,
  upsertLlmRate,
  type LlmRate,
  type UsageReport,
  type UsageSpend,
} from "../api";
import Banner from "../components/Banner";
import LoadingBlock from "../components/LoadingBlock";
import { Row, RowGroup } from "../components/SettingsRow";
import { toMessage } from "../lib/format";

/**
 * The windows the view offers. A week is "what am I spending lately", a month
 * is a billing period, three months is a trend — anything finer than a day is
 * not a question a token count answers.
 */
const WINDOWS = [
  { days: 7, label: "7 days" },
  { days: 30, label: "30 days" },
  { days: 90, label: "90 days" },
];

const DEFAULT_WINDOW = 30;

const nf = new Intl.NumberFormat();

/** A blank rate row, prefilled from whichever usage row asked for one. */
const blankRate = (providerKey = "", model = ""): LlmRate => ({
  provider_key: providerKey,
  model,
  input_per_mtok: 0,
  output_per_mtok: 0,
  currency: "USD",
  // Stamped by the backend on save; never sent from here, so a rate cannot
  // keep looking fresh across an edit that did not happen.
  updated_at: "",
});

/**
 * Money, in the currency the rate was entered in.
 *
 * Four decimal places because a single rewrite at current prices costs a
 * fraction of a cent, and rounding it to two would print "$0.00" next to a
 * real number of tokens.
 */
function formatCost(cost: number, currency: string): string {
  try {
    return new Intl.NumberFormat(undefined, {
      style: "currency",
      currency,
      maximumFractionDigits: 4,
    }).format(cost);
  } catch {
    // An unknown currency code (the field is free text) must not take the
    // page down; the number is still the useful half.
    return `${cost.toFixed(4)} ${currency}`;
  }
}

/** Why a row shows no money, in the words that say what to do about it. */
function costNote(row: UsageSpend, anyRates: boolean): string {
  if (row.unreported_calls === row.calls) {
    return "This provider does not report token counts.";
  }
  if (row.unreported_calls > 0) {
    return `${nf.format(row.unreported_calls)} of ${nf.format(
      row.calls,
    )} calls reported nothing, so these tokens are a floor.`;
  }
  if (!row.model) return "KEA was not told which model this was.";
  return anyRates
    ? `No rate for ${row.provider_key} / ${row.model}.`
    : "Add a rate below to see what this cost.";
}

export default function UsagePage() {
  const [days, setDays] = useState(DEFAULT_WINDOW);
  const [report, setReport] = useState<UsageReport | null>(null);
  const [rates, setRates] = useState<LlmRate[]>([]);
  const [draft, setDraft] = useState<LlmRate | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [next, savedRates] = await Promise.all([getUsageReport(days), listLlmRates()]);
      setReport(next);
      setRates(savedRates);
    } catch (e) {
      setError(toMessage(e));
    }
  }, [days]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const saveRate = async () => {
    if (!draft) return;
    setBusy(true);
    setError(null);
    try {
      await upsertLlmRate(draft);
      setDraft(null);
      await refresh();
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const removeRate = async (rate: LlmRate) => {
    setBusy(true);
    setError(null);
    try {
      await deleteLlmRate(rate.provider_key, rate.model);
      await refresh();
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const wipe = async () => {
    if (!window.confirm("Delete every recorded token count? This cannot be undone.")) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await clearUsage();
      await refresh();
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const totals = report?.totals ?? [];
  const tokenTotal = totals.reduce((n, r) => n + r.prompt_tokens + r.completion_tokens, 0);
  const callTotal = totals.reduce((n, r) => n + r.calls, 0);
  const unreportedTotal = totals.reduce((n, r) => n + r.unreported_calls, 0);
  // Summed per currency, because adding dollars to euros would invent a rate
  // of its own — the one thing this page refuses to do.
  const spendByCurrency = new Map<string, number>();
  for (const row of totals) {
    if (row.cost === null || !row.currency) continue;
    spendByCurrency.set(row.currency, (spendByCurrency.get(row.currency) ?? 0) + row.cost);
  }
  const busiestDay = (report?.daily ?? []).reduce(
    (max, d) => Math.max(max, d.prompt_tokens + d.completion_tokens),
    0,
  );

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>Usage</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 16 }}>
          Tokens KEA's providers reported, per feature and per model. Every number
          here came from a provider — KEA never estimates one.
        </p>
      </header>

      {error && <Banner variant="error">{error}</Banner>}

      <div className="kea-toolbar" role="group" aria-label="Time range">
        {WINDOWS.map((w) => (
          <button
            key={w.days}
            type="button"
            className="kea-segment"
            aria-pressed={days === w.days}
            onClick={() => setDays(w.days)}
          >
            {w.label}
          </button>
        ))}
      </div>

      {!report ? (
        <LoadingBlock label="Loading usage…" minHeight={120} />
      ) : totals.length === 0 ? (
        <div className="kea-card" style={{ marginTop: 16 }}>
          <p className="kea-muted" style={{ margin: 0 }}>
            No AI calls in the last {report.days} days. Rewrites, dictation clean-up
            and meeting notes all land here once they run.
          </p>
        </div>
      ) : (
        <>
          <section style={{ marginBottom: 24, marginTop: 16 }}>
            <div className="kea-card">
              <dl className="kea-detail-list">
                <dt>Calls</dt>
                <dd>{nf.format(callTotal)}</dd>
                <dt>Tokens</dt>
                <dd>{nf.format(tokenTotal)}</dd>
                <dt>Spend</dt>
                <dd>
                  {spendByCurrency.size === 0
                    ? "—"
                    : [...spendByCurrency].map(([c, v]) => formatCost(v, c)).join(" · ")}
                </dd>
              </dl>
              {unreportedTotal > 0 && (
                <p className="kea-muted" style={{ margin: "8px 0 0", fontSize: "0.8125rem" }}>
                  {nf.format(unreportedTotal)} of those calls reported no token count,
                  so the totals above are a floor rather than a total.
                </p>
              )}
              {!report.any_rates && (
                <p className="kea-muted" style={{ margin: "8px 0 0", fontSize: "0.8125rem" }}>
                  KEA ships no price list — prices change and a built-in one would
                  quietly go stale. Add the rates you are actually charged below and
                  spend appears here, next to the date you entered them.
                </p>
              )}
            </div>
          </section>

          <section style={{ marginBottom: 24 }}>
            <h2 style={{ margin: "0 0 12px" }}>Per day</h2>
            <div className="kea-table-wrap">
              <table className="kea-table">
                <caption className="kea-visually-hidden">Tokens per day</caption>
                <thead>
                  <tr>
                    <th scope="col">Day</th>
                    <th scope="col">Calls</th>
                    <th scope="col">Tokens</th>
                    <th scope="col">
                      <span className="kea-visually-hidden">Relative size</span>
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {report.daily.map((d) => {
                    const tokens = d.prompt_tokens + d.completion_tokens;
                    return (
                      <tr key={d.day}>
                        <td>{d.day}</td>
                        <td>{nf.format(d.calls)}</td>
                        <td>{nf.format(tokens)}</td>
                        <td style={{ width: "40%" }}>
                          {/* Decoration over the number beside it, so it is
                              hidden rather than read out twice. */}
                          <span
                            aria-hidden="true"
                            style={{
                              display: "block",
                              height: 8,
                              borderRadius: 4,
                              background: "var(--accent)",
                              width: busiestDay > 0 ? `${(tokens / busiestDay) * 100}%` : 0,
                              minWidth: tokens > 0 ? 2 : 0,
                            }}
                          />
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </section>

          <section style={{ marginBottom: 24 }}>
            <h2 style={{ margin: "0 0 12px" }}>Per feature and model</h2>
            <div className="kea-table-wrap">
              <table className="kea-table">
                <caption className="kea-visually-hidden">
                  Tokens and spend per feature and model
                </caption>
                <thead>
                  <tr>
                    <th scope="col">Feature</th>
                    <th scope="col">Provider</th>
                    <th scope="col">Model</th>
                    <th scope="col">Calls</th>
                    <th scope="col">In</th>
                    <th scope="col">Out</th>
                    <th scope="col">Cost</th>
                  </tr>
                </thead>
                <tbody>
                  {totals.map((row) => (
                    <tr key={`${row.feature_id}|${row.provider_key}|${row.model ?? ""}`}>
                      <td>{row.feature_id}</td>
                      <td>{row.provider_key}</td>
                      <td>{row.model ?? "—"}</td>
                      <td>{nf.format(row.calls)}</td>
                      <td>{nf.format(row.prompt_tokens)}</td>
                      <td>{nf.format(row.completion_tokens)}</td>
                      <td>
                        {row.cost !== null && row.currency ? (
                          <>
                            {formatCost(row.cost, row.currency)}
                            <span
                              className="kea-muted"
                              style={{ display: "block", fontSize: "0.75rem" }}
                            >
                              rate of {row.rate_updated_at?.slice(0, 10)}
                            </span>
                          </>
                        ) : (
                          <span className="kea-muted" style={{ fontSize: "0.8125rem" }}>
                            {costNote(row, report.any_rates)}
                          </span>
                        )}
                        {row.cost === null && row.model && (
                          <button
                            type="button"
                            className="kea-btn"
                            style={{ marginTop: 4 }}
                            disabled={busy}
                            onClick={() =>
                              setDraft(blankRate(row.provider_key, row.model ?? ""))
                            }
                          >
                            Add a rate
                          </button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>
        </>
      )}

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Rates</h2>
        <div className="kea-card">
          <p className="kea-muted" style={{ margin: "0 0 12px" }}>
            Per million tokens, as your provider publishes them. KEA never changes
            these on its own, so the date beside each one is how you know whether it
            is still true.
          </p>

          {rates.length > 0 && (
            <div className="kea-table-wrap" style={{ marginBottom: 12 }}>
              <table className="kea-table">
                <caption className="kea-visually-hidden">Saved rates</caption>
                <thead>
                  <tr>
                    <th scope="col">Provider</th>
                    <th scope="col">Model</th>
                    <th scope="col">In</th>
                    <th scope="col">Out</th>
                    <th scope="col">Entered</th>
                    <th scope="col" className="kea-table__actions">
                      <span className="kea-visually-hidden">Actions</span>
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {rates.map((rate) => (
                    <tr key={`${rate.provider_key}|${rate.model}`}>
                      <td>{rate.provider_key}</td>
                      <td>{rate.model}</td>
                      <td>
                        {rate.input_per_mtok} {rate.currency}
                      </td>
                      <td>
                        {rate.output_per_mtok} {rate.currency}
                      </td>
                      <td>{rate.updated_at.slice(0, 10)}</td>
                      <td className="kea-table__actions">
                        <button
                          type="button"
                          className="kea-btn"
                          aria-label={`Edit the rate for ${rate.provider_key} ${rate.model}`}
                          disabled={busy}
                          onClick={() => setDraft({ ...rate })}
                        >
                          Edit
                        </button>
                        <button
                          type="button"
                          className="kea-btn"
                          aria-label={`Delete the rate for ${rate.provider_key} ${rate.model}`}
                          disabled={busy}
                          onClick={() => void removeRate(rate)}
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

          {draft ? (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void saveRate();
              }}
            >
              <RowGroup aria-label="Rate">
                <Row
                  label="Provider"
                  hint="The provider you are billed by, exactly as it appears in the table above."
                >
                  <input
                    className="kea-input"
                    aria-label="Rate provider"
                    value={draft.provider_key}
                    onChange={(e) => setDraft({ ...draft, provider_key: e.target.value })}
                    placeholder="openai"
                  />
                </Row>
                <Row label="Model" hint="The model id, as the provider spells it.">
                  <input
                    className="kea-input"
                    aria-label="Rate model"
                    value={draft.model}
                    onChange={(e) => setDraft({ ...draft, model: e.target.value })}
                    placeholder="gpt-4o-mini"
                  />
                </Row>
                <Row label="Input, per million tokens">
                  <input
                    className="kea-input"
                    type="number"
                    step="0.01"
                    min="0"
                    aria-label="Input price per million tokens"
                    value={draft.input_per_mtok}
                    onChange={(e) =>
                      setDraft({ ...draft, input_per_mtok: Number(e.target.value) })
                    }
                  />
                </Row>
                <Row label="Output, per million tokens">
                  <input
                    className="kea-input"
                    type="number"
                    step="0.01"
                    min="0"
                    aria-label="Output price per million tokens"
                    value={draft.output_per_mtok}
                    onChange={(e) =>
                      setDraft({ ...draft, output_per_mtok: Number(e.target.value) })
                    }
                  />
                </Row>
                <Row label="Currency">
                  <input
                    className="kea-input"
                    aria-label="Currency"
                    value={draft.currency}
                    onChange={(e) => setDraft({ ...draft, currency: e.target.value })}
                    style={{ width: 80 }}
                  />
                </Row>
              </RowGroup>
              <div style={{ display: "flex", gap: 8, marginTop: 12, flexWrap: "wrap" }}>
                <button
                  type="submit"
                  className="kea-btn kea-btn--primary"
                  disabled={busy || !draft.provider_key.trim() || !draft.model.trim()}
                >
                  Save rate
                </button>
                <button
                  type="button"
                  className="kea-btn"
                  disabled={busy}
                  onClick={() => setDraft(null)}
                >
                  Cancel
                </button>
              </div>
            </form>
          ) : (
            <button
              type="button"
              className="kea-btn"
              disabled={busy}
              onClick={() => setDraft(blankRate())}
            >
              Add a rate
            </button>
          )}
        </div>
      </section>

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Recorded counts</h2>
        <RowGroup aria-label="Recorded counts">
          <Row
            label="Delete every recorded count"
            hint="Token counts are kept separately from conversation content, so turning History off does not remove them. This does."
          >
            <button type="button" className="kea-btn" disabled={busy} onClick={() => void wipe()}>
              Delete
            </button>
          </Row>
        </RowGroup>
      </section>
    </div>
  );
}
