import { useEffect, useState } from "react";
import {
  NOTION_PARENT_PAGE_SETTING,
  clearNotionToken,
  getNotionStatus,
  setNotionToken,
  setSetting,
  type NotionStatus,
} from "../api";
import { useSavedFlash } from "../hooks/useSavedFlash";
import { toMessage } from "../lib/format";
import { Row, RowGroup } from "./SettingsRow";

/**
 * Setting up the Notion export: a secret and a destination page.
 *
 * The two steps are spelled out rather than implied, because the second one is
 * invisible from inside KEA and its failure looks like a broken app: an
 * integration that has not been *connected to the page* gets a flat 404 from
 * Notion, which reads as "the export is broken" and not as "you have one more
 * click to make in Notion".
 */
type Props = {
  /**
   * Told after every read, so the page that owns the export button can decide
   * whether to offer it without keeping a second copy of this state.
   */
  onStatusChange?: (status: NotionStatus) => void;
};

export default function NotionSettings({ onStatusChange }: Props) {
  const [status, setStatus] = useState<NotionStatus | null>(null);
  const [tokenDraft, setTokenDraft] = useState("");
  const [pageDraft, setPageDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedKey, flash] = useSavedFlash();

  const load = () =>
    getNotionStatus()
      .then((next) => {
        setStatus(next);
        setPageDraft(next.parent_page);
        onStatusChange?.(next);
      })
      .catch((e) => setError(toMessage(e)));

  useEffect(() => {
    void load();
    // Runs once: this is the mount fetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const saveToken = async () => {
    const secret = tokenDraft.trim();
    if (busy || !secret) return;
    setBusy(true);
    setError(null);
    try {
      await setNotionToken(secret);
      setTokenDraft("");
      flash("token");
      await load();
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const forgetToken = async () => {
    setBusy(true);
    setError(null);
    try {
      await clearNotionToken();
      setTokenDraft("");
      await load();
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  // On blur, and re-read afterwards: the answer to "is this link usable?"
  // comes from the same parser the export uses, so the settings screen cannot
  // approve a link the export would refuse.
  const savePage = async () => {
    if (pageDraft === status?.parent_page) return;
    setBusy(true);
    setError(null);
    try {
      await setSetting(NOTION_PARENT_PAGE_SETTING, pageDraft.trim());
      flash("page");
      await load();
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const tokenState = status?.has_token ? "Token saved ✓" : "No token yet";

  return (
    <section style={{ marginBottom: 24 }}>
      <h2 style={{ margin: "0 0 12px" }}>Notion</h2>
      <div className="kea-card">
        <p className="kea-muted" style={{ margin: "0 0 12px", fontSize: "0.8125rem" }}>
          Exporting to Notion takes two steps, and both are done in Notion:
          create an internal integration at notion.so/my-integrations and paste
          its secret below, then open the page you want meetings filed under and
          add that integration through ⋯ → Connections. Without the second step
          Notion will say it cannot find the page.
        </p>

        <RowGroup aria-label="Notion export">
          <Row
            label="Integration secret"
            hint={
              status?.has_token
                ? "Stored in your keychain, never in the app's database."
                : "Starts with ntn_ or secret_. Stored in your keychain."
            }
          >
            <span className="kea-muted">{tokenState}</span>
            <input
              className="kea-input"
              type="password"
              aria-label="Notion integration secret"
              placeholder={status?.has_token ? "Replace the secret" : "ntn_…"}
              value={tokenDraft}
              disabled={busy}
              onChange={(e) => setTokenDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void saveToken();
              }}
              style={{ width: 200 }}
            />
            {savedKey === "token" && <span className="kea-saved">Saved ✓</span>}
            <button
              type="button"
              className="kea-btn"
              onClick={() => void saveToken()}
              disabled={busy || !tokenDraft.trim()}
            >
              Save
            </button>
            {status?.has_token && (
              <button
                type="button"
                className="kea-btn"
                onClick={() => void forgetToken()}
                disabled={busy}
              >
                Forget
              </button>
            )}
          </Row>

          <Row
            label="Page to export into"
            hint={
              status?.parent_page_error ??
              "Paste the page's link from Notion. Every export becomes a new page inside it."
            }
            tone={status?.parent_page_error ? "danger" : "muted"}
          >
            <input
              className="kea-input"
              aria-label="Notion page link"
              placeholder="https://www.notion.so/…"
              value={pageDraft}
              disabled={busy}
              onChange={(e) => setPageDraft(e.target.value)}
              onBlur={() => void savePage()}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
              }}
              style={{ width: 260 }}
            />
            {savedKey === "page" && <span className="kea-saved">Saved ✓</span>}
          </Row>
        </RowGroup>

        {error && (
          <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>{error}</p>
        )}
      </div>
    </section>
  );
}
