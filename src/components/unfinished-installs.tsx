import { Button, HStack, Text, VStack } from "@chakra-ui/react";
import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { OptionItemGroup } from "@/components/common/option-item";
import { useGlobalData } from "@/contexts/global-data";
import { useTaskContext } from "@/contexts/task";
import { useToast } from "@/contexts/toast";
import { InvokeResponse } from "@/models/response";
import { CampusModpackService } from "@/services/campus-modpack";
import {
  campusInstallProgressView,
  localizeCampusInstallError,
  visibleCampusInstalls,
} from "@/utils/campus-install-task";
import { formatByteSize } from "@/utils/string";

export const UnfinishedInstalls: React.FC = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { getInstanceList } = useGlobalData();
  const { unfinishedInstalls, refreshUnfinishedInstalls } = useTaskContext();
  const [busy, setBusy] = useState<string | null>(null);
  const running = useRef(false);
  const run = useCallback(
    async (
      action: (id: string) => Promise<InvokeResponse<unknown>>,
      id: string
    ) => {
      if (running.current) return;
      running.current = true;
      setBusy(id);
      try {
        const response = await action(id);
        if (response.status !== "success") {
          toast({
            title: t("Services.instance.installCampusModpack.error.title"),
            description: localizeCampusInstallError(
              t,
              String(response.raw_error ?? response.details ?? "")
            ),
            status: "error",
          });
        }
        if (
          response.status === "success" &&
          action === CampusModpackService.resumeCampusModpackInstall
        ) {
          toast({
            status: "info",
            title: t("Services.campusInstall.installing", {
              name:
                unfinishedInstalls.find((job) => job.instanceId === id)
                  ?.label ?? id,
            }),
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
        setBusy(null);
        refreshUnfinishedInstalls();
        getInstanceList(true);
      }
    },
    [refreshUnfinishedInstalls, getInstanceList, t, toast, unfinishedInstalls]
  );

  const visible = visibleCampusInstalls(unfinishedInstalls);
  if (visible.length === 0) return null;
  return (
    <VStack align="stretch" spacing={2}>
      <Text fontSize="xs-sm" fontWeight="bold">
        {t("DownloadTasksPage.unfinishedInstall.title")}
      </Text>
      <OptionItemGroup
        items={visible.map((job) => {
          const progress = campusInstallProgressView(job.progress);
          return (
            <HStack key={job.instanceId} justify="space-between" w="100%">
              <VStack align="start" spacing={0}>
                <Text fontSize="xs-sm">
                  {job.label} · {job.instanceName}
                </Text>
                <Text
                  fontSize="xs"
                  color={job.error ? "red.600" : undefined}
                  className={job.error ? undefined : "secondary-text"}
                >
                  {job.error
                    ? localizeCampusInstallError(t, job.error)
                    : t(
                        `DownloadTasksPage.unfinishedInstall.phase.${job.phase}`
                      )}
                </Text>
                {progress && (
                  <Text fontSize="xs" className="secondary-text">
                    {t(`DownloadTasksPage.installStage.${progress.stage}`)}
                    {progress.total > 0 &&
                      ` · ${progress.done} / ${progress.total}`}
                    {progress.bytes &&
                      ` · ${formatByteSize(progress.bytes.done)} / ${formatByteSize(progress.bytes.total)}`}
                  </Text>
                )}
              </VStack>
              <HStack>
                {job.resumable && (
                  <Button
                    size="xs"
                    variant="ghost"
                    isDisabled={busy !== null}
                    onClick={() =>
                      run(
                        CampusModpackService.resumeCampusModpackInstall,
                        job.instanceId
                      )
                    }
                  >
                    {t("DownloadTasksPage.unfinishedInstall.resume")}
                  </Button>
                )}
                <Button
                  size="xs"
                  variant="ghost"
                  colorScheme="red"
                  isDisabled={busy !== null}
                  onClick={() =>
                    run(
                      CampusModpackService.discardCampusModpackInstall,
                      job.instanceId
                    )
                  }
                >
                  {t("DownloadTasksPage.unfinishedInstall.discard")}
                </Button>
              </HStack>
            </HStack>
          );
        })}
      />
    </VStack>
  );
};
