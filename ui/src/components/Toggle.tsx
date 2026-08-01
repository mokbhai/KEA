import { useRowHintId } from "./SettingsRow";

type Props = {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
  /** Overrides the enclosing row's hint as the description. */
  "aria-describedby"?: string;
};

export default function Toggle({
  checked,
  onChange,
  label,
  disabled,
  "aria-describedby": describedBy,
}: Props) {
  const rowHintId = useRowHintId();

  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      aria-describedby={describedBy ?? rowHintId}
      className="kea-toggle"
      disabled={disabled}
      onClick={() => onChange(!checked)}
    >
      <span className="kea-toggle__thumb" aria-hidden="true" />
    </button>
  );
}
