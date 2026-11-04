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
 * Update a local import from an archive on disk: pick the file (or take a path
 * already chosen), confirm any unofficial download addresses, stage it, then
 * open the update dialog.
 * `pickingId` is the instance being processed, to show a loading state.
 */
export const useLocalModpackUpdate = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { openGenericConfirmDialog, openSharedModal } = useSharedModals();
  const [pickingId, setPickingId] = useState<string | null>(null);
  const busy = useRef(false);
  const picking = useRef(false);

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

  /**
   * Stage an archive that is already chosen and open the update dialog.
   * Resolves to whether the dialog was opened (false: cancelled or failed,
   * with the reason already shown).
   */
  const updateFromPath = useCallback(
    async (
      instanceId: string,
      instanceName: string,
      picked: string
    ): Promise<boolean> => {
      if (busy.current) return false;
      busy.current = true;
      setPickingId(instanceId);
      try {
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
          return false;
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
          return false;
        }
        if (response.data.unchanged) {
          toast({
            title: t("InstanceSyncPage.ledger.toast.sameFile"),
            status: "info",
          });
          return false;
        }
        openSharedModal("update-modpack", { instanceId, instanceName });
        return true;
      } finally {
        busy.current = false;
        setPickingId(null);
      }
    },
    [t, toast, confirmUnofficial, openSharedModal]
  );

  const updateFromFile = useCallback(
    async (instanceId: string, instanceName: string) => {
      if (busy.current || picking.current) return;
      picking.current = true;
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
        // Release the picker flag first: the staging below holds `busy`.
        picking.current = false;
        await updateFromPath(instanceId, instanceName, picked);
      } finally {
        picking.current = false;
        setPickingId(null);
      }
    },
    [t, updateFromPath]
  );
  return { updateFromFile, updateFromPath, pickingId };
};
