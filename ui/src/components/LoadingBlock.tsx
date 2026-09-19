import Spinner from "./Spinner";

type Props = {
  /** What is being fetched, e.g. "Loading settings…". */
  label: string;
  /** Reserves the height the loaded content will take, so nothing jumps. */
  minHeight?: number;
};

/** The spinner-plus-label row every page shows while its mount fetch is in flight. */
export default function LoadingBlock({ label, minHeight }: Props) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight }}>
      <Spinner size={16} />
      <span className="kea-muted">{label}</span>
    </div>
  );
}
