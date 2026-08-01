import type { ReactNode } from "react";

type RowGroupProps = {
  children: ReactNode;
  "aria-label"?: string;
};

export function RowGroup({ children, ...aria }: RowGroupProps) {
  return (
    <div className="kea-rows" role="group" {...aria}>
      {children}
    </div>
  );
}

type RowProps = {
  label: string;
  hint?: string;
  /** Use "danger" when the hint reports a failure rather than explaining the row. */
  tone?: "muted" | "danger";
  children?: ReactNode;
};

export function Row({ label, hint, tone = "muted", children }: RowProps) {
  return (
    <div className="kea-row">
      <span className="kea-row__label">
        {label}
        {hint && (
          <span
            className={`kea-row__hint${
              tone === "danger" ? " kea-row__hint--danger" : ""
            }`}
          >
            {hint}
          </span>
        )}
      </span>
      {children && <span className="kea-row__control">{children}</span>}
    </div>
  );
}
