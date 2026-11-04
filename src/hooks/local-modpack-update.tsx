import { Text, VStack } from "@chakra-ui/react";
import { open } from "@tauri-apps/plugin-dialog";
import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import { ModpackUnofficialDownloads } from "@/models/modpack-update";
import { InstanceService } from "@/services/instance";
import { ModpackUpdateService } from "@/services/modpack-update";

/**
 * Update a local import from an archive on disk: pick the file, confirm any
 * unofficial download addresses, stage it, then open the update dialog.
 * `pickingId` is the instance being processed, to show a loading state.
 */
export const useLocalModpackUpdate = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { openGenericConfirmDialog, openSharedModal } = useSharedModals();
  const [pickingId, setPickingId] = useState<string | null>(null);
  const busy = useRef(false);

  const confirmUnofficial = useCallback(
    (data: ModpackUnofficialDownloads) =>
      new Promise<boolean>((resolve) => {
        openGenericConfirmDialog({
          title: t("ImportModpackModal.unofficial.title"),
          body: (
            <VStack align="stretch" spacing={2}>
              <Text>
                {t("ImportModpackModal.unofficial.description", {
                  count: data.fileCount,
                })}
              </Text>
              {data.addresses.map((address) => (
                <Text key={address} fontSize="xs-sm" className="secondary-text">
                  {address}
                </Text>
              ))}
            </VStack>
          ),
          btnOK: t("ImportModpackModal.unofficial.trust"),
          onOKCallback: () => resolve(true),
          onCancelCallback: () => resolve(false),
        });
      }),
    [openGenericConfirmDialog, t]
  );

  const updateFromFile = useCallback(
    async (instanceId: string, instanceName: string) => {
      if (busy.current) return;
      busy.current = true;
      setPickingId(instanceId);
      try {
        const picked = await open({
          multiple: false,
          filters: [
            {
              name: t("General.dialog.filterName.modpack"),
              extensions: ["zip", "mrpack"],
            },
          ],
        });
        if (!picked) return;

        // An archive that cannot be read fails the stage step with its own error.
        const unofficial =
          await ModpackUpdateService.listLocalModpackUnofficialDownloads(
            picked
          );
        if (
          unofficial.status === "success" &&
          unofficial.data.fileCount > 0 &&
          !(await confirmUnofficial(unofficial.data))
        ) {
          return;
        }

        const meta = await InstanceService.retrieveModpackMetaInfo(picked);
        const label =
          meta.status === "success" ? meta.data.version || meta.data.name : "";
        const response = await ModpackUpdateService.stageLocalModpackUpdate(
          instanceId,
          picked,
          label
        );
        if (response.status !== "success") {
          toast({
            title: response.details || response.message,
            status: "error",
          });
          return;
        }
        if (response.data.unchanged) {
          toast({
            title: t("InstanceSyncPage.ledger.toast.sameFile"),
            status: "info",
          });
          return;
        }
        openSharedModal("update-modpack", { instanceId, instanceName });
      } finally {
        busy.current = false;
        setPickingId(null);
      }
    },
    [t, toast, confirmUnofficial, openSharedModal]
  );

  return { updateFromFile, pickingId };
};
