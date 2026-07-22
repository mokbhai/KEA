import type { ActionRow } from "../api";

type Props = {
  actions: ActionRow[];
  selectedId: number | null;
  onSelect: (id: number) => void;
};

export default function HistoryPanel({ actions, selectedId, onSelect }: Props) {
  if (actions.length === 0) {
    return (
      <p className="kea-muted" style={{ marginTop: 16 }}>
        No actions recorded yet. Run rewrite, dictation, meetings, or read-aloud to
        populate history.
      </p>
    );
  }

  return (
    <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 14 }}>
      <thead>
        <tr style={{ borderBottom: "1px solid var(--border)", textAlign: "left" }}>
          <th style={{ padding: "8px 4px" }}>ID</th>
          <th style={{ padding: "8px 4px" }}>Feature</th>
          <th style={{ padding: "8px 4px" }}>Command</th>
          <th style={{ padding: "8px 4px" }}>Engine</th>
          <th style={{ padding: "8px 4px" }}>Status</th>
        </tr>
      </thead>
      <tbody>
        {actions.map((action) => {
          const selected = selectedId === action.id;
          return (
            <tr
              key={action.id}
              onClick={() => onSelect(action.id)}
              style={{
                borderBottom: "1px solid var(--border)",
                cursor: "pointer",
                background: selected ? "var(--surface-2)" : undefined,
              }}
            >
              <td style={{ padding: "10px 4px" }}>{action.id}</td>
              <td style={{ padding: "10px 4px" }}>{action.feature_id}</td>
              <td style={{ padding: "10px 4px" }}>{action.command}</td>
              <td style={{ padding: "10px 4px" }}>{action.engine_id}</td>
              <td style={{ padding: "10px 4px" }}>{action.status}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}
