import { useEffect, useState } from "react";

import { listLlmEngines, listProviders, type Provider } from "../api";
import { enginesFor } from "../lib/engines";

/** Anything that is not a list reads as an empty one; see `useLlmChoices`. */
const asList = <T,>(value: T[] | undefined | null): T[] => (Array.isArray(value) ? value : []);

export type LlmChoices = {
  /** Engine ids this build knows how to show, in the catalog's order. */
  engines: string[];
  providers: Provider[];
};

/**
 * The two lists an "override the AI for this one thing" picker needs.
 *
 * Both app rules and rewrite presets can name their own engine, model and
 * provider, and both have to filter the backend's engine list through
 * `enginesFor("llm")` for the same reason: an engine registered but not in the
 * UI catalog has no label, and an unlabelled option in a dropdown is one the
 * user cannot choose on purpose.
 *
 * Every failure degrades to an empty list rather than an error, and so does
 * an answer that is not a list at all. The pickers are optional overrides —
 * "inherit" is always a valid answer — so a provider list that would not load
 * must leave an empty dropdown, not an unrendered page.
 */
export function useLlmChoices(): LlmChoices {
  const [engines, setEngines] = useState<string[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);

  useEffect(() => {
    listLlmEngines()
      .then((infos) => {
        const known = new Set<string>(enginesFor("llm").map((e) => e.id));
        setEngines(asList(infos).map((e) => e.id).filter((id) => known.has(id)));
      })
      .catch(() => setEngines([]));
    listProviders()
      .then((list) => setProviders(asList(list)))
      .catch(() => setProviders([]));
  }, []);

  return { engines, providers };
}
