import Banner from "./Banner";
import type { FeatureAi } from "../hooks/useFeatureAi";
import type { BlockedCause } from "../lib/featureSlot";
import type { Navigate, Page } from "../lib/nav";

/**
 * What clears each cause: a page to send the user to, or the slot picker when
 * `page` is absent. The copy lives here, with the button that shows it.
 */
const ACTIONS: Record<BlockedCause, { label: string; page?: Page }> = {
  unset: { label: "Choose…" },
  unavailable: { label: "Change…" },
  model: { label: "Open Models", page: "models" },
  credentials: { label: "Open AI Providers", page: "ai-providers" },
};

type Props = {
  ai: FeatureAi;
  onNavigate?: Navigate;
};

/**
 * The feature-page status banner: nothing at all while the feature is ready,
 * and one amber banner per blocked slot naming the problem plus the action
 * that fixes it.
 */
export default function FeatureBanner({ ai, onNavigate }: Props) {
  const blocked = (ai.statuses ?? []).filter((s) => s.blocked);

  return (
    <>
      {ai.error && <Banner variant="error">{ai.error}</Banner>}
      {blocked.map((status) => {
        const action = ACTIONS[status.blocked!.cause];
        const act = () => {
          if (action.page) onNavigate?.(action.page);
          else ai.openPicker(status.spec);
        };
        const actionable = !action.page || onNavigate;
        return (
          <Banner
            key={`${status.spec.feature}/${status.spec.slot}`}
            variant="warn"
            action={
              actionable ? (
                <button type="button" className="kea-btn" onClick={act}>
                  {action.label}
                </button>
              ) : undefined
            }
          >
            {status.spec.label} — {status.blocked!.message}
          </Banner>
        );
      })}
    </>
  );
}
