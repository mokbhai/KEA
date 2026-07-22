import type { ReactNode } from "react";

type Props = {
  variant: "ok" | "warn" | "error";
  children: ReactNode;
  action?: ReactNode;
};

export default function Banner({ variant, children, action }: Props) {
  return (
    <div
      className={`kea-banner kea-banner--${variant}`}
      role={variant === "error" ? "alert" : "status"}
    >
      <span className="kea-banner__message">{children}</span>
      {action && <span className="kea-banner__action">{action}</span>}
    </div>
  );
}
