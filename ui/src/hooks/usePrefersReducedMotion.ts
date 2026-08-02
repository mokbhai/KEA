import { useEffect, useState } from "react";

const QUERY = "(prefers-reduced-motion: reduce)";

/**
 * Tracks the OS "reduce motion" preference.
 *
 * index.css already flattens durations for everything, but that only slows an
 * animation down — it does not replace a scrolling waveform with something
 * still. Components that need a different *shape* under reduced motion read
 * the preference here.
 */
export function usePrefersReducedMotion(): boolean {
  const [mq] = useState<MediaQueryList | null>(() =>
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia(QUERY)
      : null,
  );
  const [reduced, setReduced] = useState(() => mq?.matches ?? false);

  useEffect(() => {
    if (!mq) return;
    setReduced(mq.matches);
    const onChange = () => setReduced(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [mq]);

  return reduced;
}

export default usePrefersReducedMotion;
