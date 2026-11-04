import {
  ModpackConflict,
  ModpackConflictChoice,
} from "@/models/modpack-update";

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

/**
 * What an unanswered conflict (or block) is: the new version is applied. The
 * backend's `ConflictChoice::default()` is the same, and `withDefaultChoices`
 * sends it explicitly anyway.
 */
export const DEFAULT_CONFLICT_CHOICE: ModpackConflictChoice = "takeRemote";

export const choiceOf = (
  choices: Record<string, ModpackConflictChoice>,
  key: string
): ModpackConflictChoice => choices[key] ?? DEFAULT_CONFLICT_CHOICE;

/** The key a conflict is answered by: its path, or `path#index` for a block. */
export const conflictChoiceKeys = (conflict: ModpackConflict): string[] =>
  conflict.hunks.length === 0
    ? [conflict.path]
    : conflict.hunks.map((_, index) => `${conflict.path}#${index}`);

/** The answers to submit: every conflict answered, by default or by the player. */
export const withDefaultChoices = (
  conflicts: ModpackConflict[],
  choices: Record<string, ModpackConflictChoice>
): Record<string, ModpackConflictChoice> => {
  const result: Record<string, ModpackConflictChoice> = {};
  for (const conflict of conflicts) {
    for (const key of conflictChoiceKeys(conflict)) {
      result[key] = choiceOf(choices, key);
    }
  }
  return result;
};
