import {
  Children,
  cloneElement,
  createContext,
  isValidElement,
  useContext,
  useId,
  type ReactElement,
  type ReactNode,
} from "react";

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

/**
 * The id of the current row's hint, or undefined when the row has none.
 * Components rendered as a row's control read this so the hint — which is
 * visual-only otherwise — is announced with the control.
 */
const RowHintContext = createContext<string | undefined>(undefined);

export function useRowHintId(): string | undefined {
  return useContext(RowHintContext);
}

/**
 * Native form controls written inline as row children can't call the hook, so
 * they get the association by cloning instead. Anything that already carries
 * its own aria-describedby keeps it.
 */
const DESCRIBABLE = new Set(["input", "select", "textarea"]);

function describeControls(children: ReactNode, hintId: string | undefined): ReactNode {
  if (!hintId) return children;
  return Children.map(children, (child) => {
    if (!isValidElement(child)) return child;
    if (typeof child.type !== "string" || !DESCRIBABLE.has(child.type)) return child;
    const props = child.props as { "aria-describedby"?: string };
    if (props["aria-describedby"]) return child;
    return cloneElement(child as ReactElement<{ "aria-describedby"?: string }>, {
      "aria-describedby": hintId,
    });
  });
}

type RowProps = {
  label: string;
  hint?: string;
  /** Use "danger" when the hint reports a failure rather than explaining the row. */
  tone?: "muted" | "danger";
  children?: ReactNode;
};

export function Row({ label, hint, tone = "muted", children }: RowProps) {
  const generatedId = useId();
  const hintId = hint ? generatedId : undefined;

  return (
    <div className="kea-row">
      <span className="kea-row__label">
        {label}
        {hint && (
          <span
            id={hintId}
            className={`kea-row__hint${
              tone === "danger" ? " kea-row__hint--danger" : ""
            }`}
          >
            {hint}
          </span>
        )}
      </span>
      {children && (
        <span className="kea-row__control">
          <RowHintContext.Provider value={hintId}>
            {describeControls(children, hintId)}
          </RowHintContext.Provider>
        </span>
      )}
    </div>
  );
}
