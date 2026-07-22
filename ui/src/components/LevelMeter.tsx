type Props = {
  level: number;
};

export default function LevelMeter({ level }: Props) {
  const clamped = Math.max(0, Math.min(1, level));

  return (
    <div
      role="meter"
      aria-valuenow={clamped}
      aria-valuemin={0}
      aria-valuemax={1}
      aria-label="Microphone level"
      style={{
        width: 120,
        height: 8,
        borderRadius: 4,
        background: "var(--surface-2)",
        overflow: "hidden",
      }}
    >
      <div
        style={{
          width: `${clamped * 100}%`,
          height: "100%",
          background: "var(--accent)",
          borderRadius: 4,
          transition: "width 50ms linear",
        }}
      />
    </div>
  );
}
