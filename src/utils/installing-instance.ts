import type { InstanceSummary } from "@/models/instance/misc";
import type { ModpackUpdateCheck } from "@/models/modpack-update";

export const isInstallingInstance = (instance?: InstanceSummary): boolean =>
  instance?.campusInstallPending === true;

export const shouldCheckInstance = (instance?: InstanceSummary): boolean =>
  !!instance?.origin && !isInstallingInstance(instance);

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
