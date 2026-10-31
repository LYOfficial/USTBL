// Grouping of an update plan's file changes for the preview (plan
// 《整合包本地改动明细》§11.1): what the player ignored gets its own group.
import { ModpackFileChange, ModpackUpdatePlan } from "@/models/modpack-update";

export interface PlanChangeGroups {
  /** Ignored changes this update puts back to the modpack's content. */
  restored: ModpackFileChange[];
  add: ModpackFileChange[];
  update: ModpackFileChange[];
  delete: ModpackFileChange[];
  disable: ModpackFileChange[];
  enable: ModpackFileChange[];
  retire: ModpackFileChange[];
  autoMerged: ModpackFileChange[];
}

/** A restored change shows only in the restored group, never in its kind's. */
export const groupPlanChanges = (
  plan: Pick<ModpackUpdatePlan, "changes" | "autoMerged">
): PlanChangeGroups => {
  const plain = plan.changes.filter((change) => !change.restored);
  const kind = (name: ModpackFileChange["kind"]) =>
    plain.filter((change) => change.kind === name);
  return {
    restored: [...plan.changes, ...plan.autoMerged].filter(
      (change) => change.restored
    ),
    add: kind("add"),
    update: kind("update"),
    delete: kind("delete"),
    disable: kind("disable"),
    enable: kind("enable"),
    retire: kind("retire"),
    autoMerged: plan.autoMerged.filter((change) => !change.restored),
  };
};
