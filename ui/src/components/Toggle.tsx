type Props = {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
};

export default function Toggle({ checked, onChange, label, disabled }: Props) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className="kea-toggle"
      disabled={disabled}
      onClick={() => onChange(!checked)}
    >
      <span className="kea-toggle__thumb" aria-hidden="true" />
    </button>
  );
}
