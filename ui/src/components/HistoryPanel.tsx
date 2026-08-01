import type { ActionRow } from "../api";

type Props = {
  actions: ActionRow[];
  selectedId: number | null;
  onSelect: (id: number) => void;
};

/** Status text carries meaning, so it gets a token colour rather than --accent. */
export function statusClass(status: string): string {
  if (status === "ok" || status === "success") return "kea-status--ok";
  if (status === "error" || status === "failed") return "kea-status--error";
  return "kea-status--muted";
}

export default function HistoryPanel({ actions, selectedId, onSelect }: Props) {
  if (actions.length === 0) {
    return (
      <div className="kea-card">
        <p className="kea-muted" style={{ margin: 0 }}>
          No actions recorded yet. Run rewrite, dictation, meetings, or read-aloud to
          populate history.
        </p>
      </div>
    );
  }

  return (
    <div className="kea-table-wrap">
      <table className="kea-table">
        <caption className="kea-visually-hidden">Recorded actions</caption>
        <thead>
          <tr>
            <th scope="col">ID</th>
            <th scope="col">Feature</th>
            <th scope="col">Command</th>
            <th scope="col">Engine</th>
            <th scope="col">Status</th>
          </tr>
        </thead>
        <tbody>
          {actions.map((action) => {
            const selected = selectedId === action.id;
            return (
              <tr
                key={action.id}
                aria-current={selected ? "true" : undefined}
                onClick={() => onSelect(action.id)}
              >
                <td>
                  {/* The row stays clickable; this is the part that takes a tab
                      stop, so the detail is reachable without a mouse. */}
                  <button
                    type="button"
                    className="kea-table__select"
                    aria-label={`Show action ${action.id}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      onSelect(action.id);
                    }}
                  >
                    {action.id}
                  </button>
                </td>
                <td>{action.feature_id}</td>
                <td>{action.command}</td>
                <td>{action.engine_id}</td>
                <td className={statusClass(action.status)}>{action.status}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
