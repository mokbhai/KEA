import { screen } from "@testing-library/react";

/**
 * Whether a feature page's "Enable …" switch comes after its title and before
 * everything else the page offers. A page that builds its own header instead of
 * the shared one fails here.
 */
export function switchSitsUnderTitle(name: string): boolean {
  const title = screen.getByRole("heading", { level: 1, name });
  const toggle = screen.getByRole("switch", { name: `Enable ${name}` });
  const header = title.closest("header");
  return (
    header !== null &&
    header.contains(toggle) &&
    Boolean(title.compareDocumentPosition(toggle) & Node.DOCUMENT_POSITION_FOLLOWING) &&
    header.parentElement?.firstElementChild === header
  );
}
