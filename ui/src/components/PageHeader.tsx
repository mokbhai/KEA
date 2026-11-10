import type { ReactNode } from "react";

type Props = {
  title: string;
  /** One or two sentences on what the page is for, set close under the title. */
  summary?: ReactNode;
  /** The page's main controls, on the title's line. */
  actions?: ReactNode;
  /** Use when a toolbar follows, so it reads as part of the header. */
  tight?: boolean;
  /** Anything else that belongs to the title, such as a feature's switch. */
  children?: ReactNode;
};

/**
 * The top of every page: the title, then what belongs to it, then the content.
 * One component so the spacing between the three is the same on every page.
 */
export default function PageHeader({ title, summary, actions, tight, children }: Props) {
  return (
    <header className={`kea-page-header${tight ? " kea-page-header--tight" : ""}`}>
      <div className="kea-page-header__title">
        <h1>{title}</h1>
        {actions && <div className="kea-page-header__actions">{actions}</div>}
      </div>
      {summary && <p className="kea-page-header__summary">{summary}</p>}
      {children}
    </header>
  );
}
