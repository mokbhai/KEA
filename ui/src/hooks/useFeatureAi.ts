import { useCallback, useEffect, useState } from "react";
import { loadSlotStatuses, type SlotSpec, type SlotStatus } from "../lib/featureSlot";

export type FeatureAi = {
  /** null while the first load is in flight. */
  statuses: SlotStatus[] | null;
  error: string | null;
  reload: () => void;
  /** The slot whose picker is open, if any. */
  pickerSpec: SlotSpec | null;
  openPicker: (spec: SlotSpec) => void;
  closePicker: () => void;
};

/**
 * Loads the AI status of every slot a feature page depends on, and owns the
 * defaults-picker state so the status banner and the AI card can both drive it.
 * `specs` must be a stable (module-level) array.
 */
export function useFeatureAi(specs: SlotSpec[]): FeatureAi {
  const [statuses, setStatuses] = useState<SlotStatus[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const [pickerSpec, setPickerSpec] = useState<SlotSpec | null>(null);

  useEffect(() => {
    let cancelled = false;
    loadSlotStatuses(specs)
      .then((report) => {
        if (cancelled) return;
        setStatuses(report.statuses);
        // Say what actually happened rather than guessing at a cause: with a
        // failed lookup the report carries no diagnoses at all.
        setError(
          report.error
            ? `Couldn't check the AI setup for this feature: ${report.error}`
            : null,
        );
      })
      .catch((e) => {
        if (cancelled) return;
        setStatuses([]);
        setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [specs, nonce]);

  const reload = useCallback(() => setNonce((n) => n + 1), []);
  const openPicker = useCallback((spec: SlotSpec) => setPickerSpec(spec), []);
  const closePicker = useCallback(() => setPickerSpec(null), []);

  return { statuses, error, reload, pickerSpec, openPicker, closePicker };
}
