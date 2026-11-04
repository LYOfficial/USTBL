import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useGlobalData } from "@/contexts/global-data";
import { useTaskContext } from "@/contexts/task";
import { useToast } from "@/contexts/toast";
import { useRemoveInstanceFromGroups } from "@/hooks/instance-groups";
import { CampusModpackService } from "@/services/campus-modpack";
import { localizeCampusInstallError } from "@/utils/campus-install-task";

/** Abandon a first install, running or not. Resolves to whether it worked. */
export const useDiscardCampusInstall = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { getInstanceList } = useGlobalData();
  const removeFromGroups = useRemoveInstanceFromGroups();
  const { refreshUnfinishedInstalls } = useTaskContext();
  const [discarding, setDiscarding] = useState(false);
  const running = useRef(false);

  const discard = useCallback(
    async (instanceId: string): Promise<boolean> => {
      if (running.current) return false;
      running.current = true;
      setDiscarding(true);
      let succeeded = false;
      try {
        const response =
          await CampusModpackService.discardCampusModpackInstall(instanceId);
        if (response.status === "success") {
          succeeded = true;
          removeFromGroups(instanceId);
        } else {
          toast({
            title: t("Services.instance.installCampusModpack.error.title"),
            description: localizeCampusInstallError(
              t,
              String(response.raw_error ?? response.details ?? "")
            ),
            status: "error",
          });
        }
      } catch (error) {
        toast({
          title: t("Services.instance.installCampusModpack.error.title"),
          description: localizeCampusInstallError(t, String(error)),
          status: "error",
        });
      } finally {
        running.current = false;
        setDiscarding(false);
        refreshUnfinishedInstalls();
        getInstanceList(true);
      }
      return succeeded;
    },
    [getInstanceList, refreshUnfinishedInstalls, removeFromGroups, t, toast]
  );

  return { discard, discarding };
};
