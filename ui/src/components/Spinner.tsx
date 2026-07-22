type Props = {
  size?: number;
};

export default function Spinner({ size = 16 }: Props) {
  return (
    <span
      className="kea-spinner"
      role="status"
      aria-label="Loading"
      style={{ width: size, height: size }}
    />
  );
}
