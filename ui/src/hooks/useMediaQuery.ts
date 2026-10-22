import { useEffect, useState } from "react";

/**
 * Whether a CSS media query currently matches.
 *
 * Reads `matchMedia` lazily and tolerates its absence (older test DOMs, some
 * previews), in which case the query reads as not matching. The same pattern
 * as `usePrefersReducedMotion`, kept generic so a layout can name its own
 * breakpoint.
 */
export function useMediaQuery(query: string): boolean {
  const [mq] = useState<MediaQueryList | null>(() =>
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia(query)
      : null,
  );
  const [matches, setMatches] = useState(() => mq?.matches ?? false);

  useEffect(() => {
    if (!mq) return;
    // Re-sync: the viewport can change between the seeding render and this
    // effect, and that change fires no event we are subscribed to yet.
    setMatches(mq.matches);
    const onChange = () => setMatches(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [mq]);

  return matches;
}

/** The meetings workspace's two-pane breakpoint: below it, one pane at a time. */
export const MEETINGS_NARROW_QUERY = "(max-width: 720px)";
