import { useEffect, useRef } from "react";
import Spinner from "./Spinner";

type Props = {
  content: string;
  loading?: boolean;
  /** Keep the newest lines in view as the tail grows. */
  follow?: boolean;
};

export default function LogsViewer({ content, loading, follow }: Props) {
  const pane = useRef<HTMLPreElement>(null);

  useEffect(() => {
    if (!follow || !pane.current) return;
    pane.current.scrollTop = pane.current.scrollHeight;
  }, [content, follow]);

  return (
    <pre className="kea-logs" ref={pane}>
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
