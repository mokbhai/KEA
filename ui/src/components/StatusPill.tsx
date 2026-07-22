type Variant = "progress" | "error";

type Props = {
  message: string | null;
  variant: Variant | null;
};

const colors: Record<Variant, { bg: string; fg: string }> = {
  progress: { bg: "var(--surface-2)", fg: "var(--accent)" },
  error: { bg: "var(--surface-2)", fg: "var(--danger)" },
};

export default function StatusPill({ message, variant }: Props) {
  if (!message || !variant) return null;

  const { bg, fg } = colors[variant];

  return (
    <div
      role="status"
      style={{
        position: "fixed",
        bottom: 16,
        left: "50%",
        transform: "translateX(-50%)",
        padding: "8px 16px",
        borderRadius: 999,
        background: bg,
        color: fg,
        fontSize: 14,
        fontWeight: 500,
        border: variant === "error" ? "1px solid var(--danger)" : "1px solid var(--border)",
        boxShadow: "0 2px 8px color-mix(in srgb, var(--text) 12%, transparent)",
        maxWidth: "min(90vw, 480px)",
        textAlign: "center",
      }}
    >
      {message}
    </div>
  );
}
