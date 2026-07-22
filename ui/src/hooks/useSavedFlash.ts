import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Tracks which row just saved so it can show an inline "Saved ✓" that fades
 * after a moment. Keyed so a page with many save-on-change rows needs one hook.
 */
export function useSavedFlash(timeoutMs = 2000): [string | null, (key: string) => void] {
  const [savedKey, setSavedKey] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const flash = useCallback(
    (key: string) => {
      setSavedKey(key);
      clearTimeout(timer.current);
      timer.current = setTimeout(() => setSavedKey(null), timeoutMs);
    },
    [timeoutMs],
  );

  useEffect(() => () => clearTimeout(timer.current), []);

  return [savedKey, flash];
}
