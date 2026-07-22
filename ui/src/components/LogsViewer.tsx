import Spinner from "./Spinner";

type Props = {
  content: string;
  loading?: boolean;
};

export default function LogsViewer({ content, loading }: Props) {
  return (
    <pre
      style={{
        margin: 0,
        padding: 16,
        background: "var(--surface-2)",
        color: "var(--text)",
        border: "1px solid var(--border)",
        borderRadius: 8,
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
        fontSize: 12,
        lineHeight: 1.5,
        overflow: "auto",
        maxHeight: "min(70vh, 640px)",
        whiteSpace: "pre-wrap",
        wordBreak: "break-word",
      }}
    >
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
