import { useCallback, useRef, useState } from "react";
import type { ModelDownloadProgress } from "../api";
import {
  applyDefaultChoice,
  cancelOptionDownload,
  startOptionDownload,
  type BindingTarget,
  type Capability,
  type CapabilityOption,
} from "../lib/capabilityDefaults";
import { useModelDownloads } from "./useModelDownloads";

/** One "make this the default" request, including where it writes. */
export type ActivationRequest = {
  capability: Capability;
  option: CapabilityOption;
  /** Cloud voice to store alongside a TTS choice, when the option has one. */
  voice: string | null;
  target: BindingTarget;
};

export type PendingActivation = {
  /** The choice waiting on a model download, if any. */
  pending: ActivationRequest | null;
  /** Option id of the most recent successful activation ("Saved ✓"). */
  savedId: string | null;
  error: string | null;
  /** Download progress for the pending model, keyed by model id. */
  progressById: Map<string, ModelDownloadProgress>;
  /** Downloads first when needed, then writes the binding. */
  start: (request: ActivationRequest) => Promise<void>;
  /** Abandons the pending download and frees the picker for another pick. */
  cancel: () => Promise<void>;
  /** Drops the saved/error feedback, keeping any pending download. */
  clearFeedback: () => void;
  setError: (message: string | null) => void;
};

/**
 * Owns the "download a model, then activate it" flow.
 *
 * It lives above the DefaultsPicker on purpose: a download outlives the
 * popover, and when the picker owned this state, closing it mid-download
 * dropped the activation silently — the bytes landed but no default was ever
 * set. Held by the page (or the card) instead, the choice is applied when the
 * download finishes whether or not the picker is still on screen, and
 * `onApplied` refreshes the surface behind it.
 *
 * Known residual: the state lives no higher than the page, so navigating away
 * mid-download still drops the activation (the download itself continues, and
 * the model shows up as installed). Lifting it to app level or to a module
 * store would close that gap; issue #3 asked only that closing the popover
 * stop losing the choice.
 */
export function usePendingActivation(onApplied?: () => void): PendingActivation {
  const [pending, setPending] = useState<ActivationRequest | null>(null);
  const [savedId, setSavedId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Mirrors of the state above, written imperatively in `start`: a download
  // event can arrive before React re-renders, and both the event filter and
  // the completion handler have to see the new pending request already.
  const pendingRef = useRef<ActivationRequest | null>(null);
  const catalogRef = useRef<Set<string>>(new Set());
  const onAppliedRef = useRef(onApplied);
  onAppliedRef.current = onApplied;

  const forget = useCallback(() => {
    pendingRef.current = null;
    catalogRef.current.clear();
    setPending(null);
  }, []);

  const apply = useCallback(
    async (request: ActivationRequest) => {
      try {
        await applyDefaultChoice(
          request.capability,
          request.option,
          request.capability === "tts" ? request.voice : null,
          request.target,
        );
        forget();
        setSavedId(request.option.id);
        onAppliedRef.current?.();
      } catch (e) {
        forget();
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [forget],
  );

  const { progressById } = useModelDownloads({
    // A stable Set, mutated in place, so events for a just-started download
    // are not filtered out by a catalog snapshot from the previous render.
    catalogIds: catalogRef.current,
    onComplete: (modelId) => {
      const request = pendingRef.current;
      if (request?.option.model !== modelId) return;
      void apply({ ...request, option: { ...request.option, installed: true } });
    },
    onError: (modelId, message) => {
      if (pendingRef.current?.option.model !== modelId) return;
      forget();
      setError(`${modelId}: ${message}`);
    },
  });

  const start = useCallback(
    async (request: ActivationRequest) => {
      if (pendingRef.current) return;
      setError(null);
      setSavedId(null);
      const { option } = request;
      if (option.downloadKind && option.model && !option.installed) {
        pendingRef.current = request;
        catalogRef.current.clear();
        catalogRef.current.add(option.model);
        setPending(request);
        try {
          await startOptionDownload(option);
        } catch (e) {
          forget();
          setError(e instanceof Error ? e.message : String(e));
        }
        return;
      }
      await apply(request);
    },
    [apply, forget],
  );

  /**
   * Gives up on the pending download.
   *
   * The local state is dropped whether or not the backend acknowledges: the
   * case this exists for is a download that stopped reporting altogether, and
   * refusing to clear until the backend answers would leave the user in the
   * very state they are trying to escape — `start` ignores every click while a
   * request is pending, so a stuck request means no pick can be made at all.
   */
  const cancel = useCallback(async () => {
    const request = pendingRef.current;
    if (!request) return;
    forget();
    setError(null);
    try {
      await cancelOptionDownload(request.option);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [forget]);

  const clearFeedback = useCallback(() => {
    setSavedId(null);
    setError(null);
  }, []);

  return { pending, savedId, error, progressById, start, cancel, clearFeedback, setError };
}
