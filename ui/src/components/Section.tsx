import type { ReactNode } from "react";

type Props = {
  title: string;
  /** One or two sentences under the heading, for a section that needs explaining. */
  note?: ReactNode;
  /** Controls that act on the whole section, on the heading's line. */
  actions?: ReactNode;
  children: ReactNode;
};

/**
 * One titled block of a page. Every page spaces and sizes its sections through
 * this, so the gap above a heading, the gap under it and the heading's weight
 * are the same wherever you are in the app.
 */
export default function Section({ title, note, actions, children }: Props) {
  return (
    <section className="kea-section">
      <div className="kea-section__head">
        <h2>{title}</h2>
        {actions && <div className="kea-section__actions">{actions}</div>}
      </div>
      {note && <p className="kea-section__note">{note}</p>}
      {children}
    </section>
  );
}

/** Buttons or a footnote that belong to the section, under its content. */
export function SectionFooter({ children }: { children: ReactNode }) {
  return <div className="kea-section__footer">{children}</div>;
}
