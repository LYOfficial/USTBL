import type { InstanceSummary } from "@/models/instance/misc";
import type { ModpackUpdateCheck } from "@/models/modpack-update";

export const isInstallingInstance = (instance?: InstanceSummary): boolean =>
  instance?.campusInstallPending === true;

export const shouldCheckInstance = (instance?: InstanceSummary): boolean =>
  !!instance?.origin && !isInstallingInstance(instance);

/**
 * One campus project listing replaces a per-instance request once two or more
 * followed campus instances are about to be checked. Paused instances make no
 * request, so they do not count.
 */
export const shouldPrimeCampusListing = (
  instances: InstanceSummary[]
): boolean =>
  instances.filter(
    (instance) =>
      shouldCheckInstance(instance) &&
      instance.origin?.platform === "campus" &&
      instance.origin.follow
  ).length >= 2;

export const installingCheck = (
  instance: InstanceSummary
): ModpackUpdateCheck => ({
  status: "installing",
  follow: instance.origin?.follow ?? false,
  currentLabel: instance.modpackVersion,
  canRollBack: false,
});

export const instanceNavigationGroups = (instances: InstanceSummary[]) => {
  const installing = instances.filter(isInstallingInstance);
  const all = instances.filter((instance) => !isInstallingInstance(instance));
  return {
    all,
    installing,
    allCount: all.length,
    installingCount: installing.length,
  };
};
