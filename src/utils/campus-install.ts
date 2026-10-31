import { TFunction } from "i18next";
import { useToast } from "@/contexts/toast";
import { CampusModpackService } from "@/services/campus-modpack";
import { localizeCampusInstallError } from "@/utils/campus-install-task";

/** Submit once; the backend persists the wait for the client and the install. */
export const runCampusInstall = (
  install: { instanceId: string; label: string },
  toast: ReturnType<typeof useToast>,
  t: TFunction
) => {
  toast({
    status: "info",
    title: t("Services.campusInstall.installing", { name: install.label }),
  });
  return CampusModpackService.installCampusModpack(install.instanceId).then(
    (response) => {
      if (response.status !== "success") {
        toast({
          status: "error",
          title: t("Services.instance.installCampusModpack.error.title"),
          description: localizeCampusInstallError(
            t,
            String(response.raw_error ?? response.details ?? "")
          ),
        });
      }
      return response;
    }
  );
};
