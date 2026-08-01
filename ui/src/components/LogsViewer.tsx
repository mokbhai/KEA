import Spinner from "./Spinner";

type Props = {
  content: string;
  loading?: boolean;
};

export default function LogsViewer({ content, loading }: Props) {
  return (
    <pre className="kea-logs">
      {loading ? (
        <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
          <Spinner size={14} /> Loading logs…
        </span>
      ) : (
        content || "(Log file is empty or not created yet.)"
      )}
    </pre>
  );
}
