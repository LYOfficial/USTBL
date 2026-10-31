import { ModpackConflictChoice } from "@/models/modpack-update";

/** Conflict choices remembered per instance while the launcher runs. */
export type RememberedChoices = Map<
  string,
  { planHash: string; choices: Record<string, ModpackConflictChoice> }
>;

export const rememberedChoices: RememberedChoices = new Map();
export const failedUpdatePlans = new Map<string, string>();

/**
 * The choices the dialog should show when `newPlanHash` is prepared. After a
 * failed apply (`recheckAfterFailure`) the player's own answers survive only
 * while the plan is still the one they were made for; anything else clears
 * them. A plain open restores what the player picked the last time this exact
 * plan was shown. The toast flag tells the dialog to announce the clear.
 */
export function resolvePlanChoices(params: {
  instanceId: string;
  newPlanHash: string;
  /** The failed plan's hash when this prepare is the re-check after a failed apply. */
  failedPlanHash?: string | null;
  recheckAfterFailure: boolean;
  remembered: RememberedChoices;
  currentChoices: Record<string, ModpackConflictChoice>;
}): {
  choices: Record<string, ModpackConflictChoice>;
  toastPlanChanged: boolean;
} {
  if (params.recheckAfterFailure) {
    if (params.failedPlanHash === params.newPlanHash) {
      return { choices: params.currentChoices, toastPlanChanged: false };
    }
    return { choices: {}, toastPlanChanged: true };
  }
  const remembered = params.remembered.get(params.instanceId);
  if (remembered && remembered.planHash === params.newPlanHash) {
    return { choices: remembered.choices, toastPlanChanged: false };
  }
  return { choices: {}, toastPlanChanged: false };
}
