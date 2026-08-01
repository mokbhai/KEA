import Banner from "./Banner";
import type { FeatureAi } from "../hooks/useFeatureAi";

export type FixNavigate = (page: "ai-providers" | "models") => void;

type Props = {
  ai: FeatureAi;
  onNavigate?: FixNavigate;
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
        const reason = status.blocked!;
        const act = () => {
          if (reason.fix === "picker") ai.openPicker(status.spec);
          else onNavigate?.(reason.fix);
        };
        const actionable = reason.fix === "picker" || onNavigate;
        return (
          <Banner
            key={`${status.spec.feature}/${status.spec.slot}`}
            variant="warn"
            action={
              actionable ? (
                <button type="button" className="kea-btn" onClick={act}>
                  {reason.actionLabel}
                </button>
              ) : undefined
            }
          >
            {status.spec.label} — {reason.message}
          </Banner>
        );
      })}
    </>
  );
}
