import { useEffect, useRef, useState } from "react";
import {
  onModelDownloadComplete,
  onModelDownloadError,
  onModelDownloadProgress,
  type ModelDownloadProgress,
} from "../api";

type Options = {
  /** Only events for these model ids are surfaced (other catalogs are ignored). */
  catalogIds: Set<string>;
  onComplete?: (modelId: string) => void;
  onError?: (modelId: string, message: string) => void;
};

/**
 * Subscribes to model download events and tracks progress per model id, so
 * concurrent downloads don't fight over a single progress slot.
 */
export function useModelDownloads({ catalogIds, onComplete, onError }: Options) {
  const [progressById, setProgressById] = useState<Map<string, ModelDownloadProgress>>(
    () => new Map(),
  );
  const catalogRef = useRef(catalogIds);
  catalogRef.current = catalogIds;
  const onCompleteRef = useRef(onComplete);
  onCompleteRef.current = onComplete;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  useEffect(() => {
    const clearProgress = (modelId: string) =>
      setProgressById((prev) => {
        const next = new Map(prev);
        next.delete(modelId);
        return next;
      });

    const unsubs = Promise.all([
      onModelDownloadProgress((p) => {
        if (!catalogRef.current.has(p.model_id)) return;
        setProgressById((prev) => new Map(prev).set(p.model_id, p));
      }),
      onModelDownloadComplete((modelId) => {
        if (!catalogRef.current.has(modelId)) return;
        clearProgress(modelId);
        onCompleteRef.current?.(modelId);
      }),
      onModelDownloadError((modelId, message) => {
        if (!catalogRef.current.has(modelId)) return;
        clearProgress(modelId);
        onErrorRef.current?.(modelId, message);
      }),
    ]);

    return () => {
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, []);

  return { progressById };
}
