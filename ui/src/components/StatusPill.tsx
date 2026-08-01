type Variant = "progress" | "error";

type Props = {
  message: string | null;
  variant: Variant | null;
};

export default function StatusPill({ message, variant }: Props) {
  if (!message || !variant) return null;

  return (
    <div
      role="status"
      className={`kea-float kea-status-pill kea-status-pill--${variant}`}
    >
      {message}
    </div>
  );
}
