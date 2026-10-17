import { TFunction } from "i18next";
import { useToast } from "@/contexts/toast";
import { CampusModpackService } from "@/services/campus-modpack";

/**
 * Campus installs that created their empty instance and are waiting for the
 * game client to finish downloading, keyed by instance name. Kept in memory
 * only: if the launcher restarts first, the instance stays empty and follows
 * the modpack, so the update check offers the install again.
 */
export interface PendingCampusInstall {
  instanceId: string;
  label: string;
}

const pending = new Map<string, PendingCampusInstall>();

export const registerPendingCampusInstall = (
  instanceName: string,
  install: PendingCampusInstall
) => {
  pending.set(instanceName, install);
};

export const takePendingCampusInstall = (
  instanceName: string
): PendingCampusInstall | undefined => {
  const install = pending.get(instanceName);
  pending.delete(instanceName);
  return install;
};

/** Put the modpack files into the instance and report progress in one toast. */
export const runCampusInstall = (
  install: PendingCampusInstall,
  toast: ReturnType<typeof useToast>,
  t: TFunction,
  onFinished: () => void
) => {
  const toastId = toast({
    status: "loading",
    title: t("Services.campusInstall.installing", { name: install.label }),
  });
  const stopListening = CampusModpackService.onInstallProgress((payload) => {
    if (payload.instanceId !== install.instanceId) return;
    const { stage, done, total } = payload;
    toast.update(toastId, {
      description: t(`Services.campusInstall.stage.${stage}`, { done, total }),
    });
  });
  CampusModpackService.installCampusModpack(install.instanceId)
    .then((response) => {
      toast.close(toastId);
      if (response.status === "success") {
        toast({
          status: "success",
          title: response.message,
          description: install.label,
        });
      } else {
        toast({
          status: "error",
          title: response.message,
          description: response.details,
        });
      }
    })
    .finally(() => {
      stopListening();
      onFinished();
    });
};
