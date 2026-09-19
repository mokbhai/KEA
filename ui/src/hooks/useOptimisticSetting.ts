import { useCallback, useState } from "react";

import { toMessage } from "../lib/format";
import { useSavedFlash } from "./useSavedFlash";

type Options<T> = {
  /** The value to show until the mount fetch lands. */
  initial: T;
  /** Writes the whole settings object back. */
  persist: (next: T) => Promise<unknown>;
  /**
   * Re-reads the stored settings just before writing. Needed when the backend
   * saves the object whole and other fields may have moved underneath us —
   * without it, writing a stale copy of those fields quietly reverts them.
   */
  reread?: () => Promise<T>;
  /** How long the "Saved ✓" stays up. */
  flashMs?: number;
};

export type OptimisticSetting<T> = {
  value: T;
  busy: boolean;
  error: string | null;
  savedKey: string | null;
  /**
   * Shows `patch` immediately, writes it, and puts the previous value back if
   * the write is refused — so the UI never keeps displaying a value the
   * backend rejected.
   */
  save: (patch: Partial<T>, key: string) => Promise<void>;
  /** Local edit with no write, for inputs that only persist on blur. */
  setValue: (value: T) => void;
  /** Lets the mount fetch report through the same error line as the saves. */
  setError: (error: string | null) => void;
};

/**
 * Optimistic save-with-rollback for a settings object: the shared shape behind
 * every "flip the toggle, write it, undo it if the write fails" handler.
 */
export function useOptimisticSetting<T extends object>({
  initial,
  persist,
  reread,
  flashMs,
}: Options<T>): OptimisticSetting<T> {
  const [value, setValue] = useState<T>(initial);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedKey, flash] = useSavedFlash(flashMs);

  const save = useCallback(
    async (patch: Partial<T>, key: string) => {
      const previous = value;
      setValue({ ...previous, ...patch });
      setBusy(true);
      setError(null);
      try {
        const base = reread ? await reread() : previous;
        await persist({ ...base, ...patch });
        flash(key);
      } catch (e) {
        setValue(previous);
        setError(toMessage(e));
      } finally {
        setBusy(false);
      }
    },
    [flash, persist, reread, value],
  );

  return { value, busy, error, savedKey, save, setValue, setError };
}
