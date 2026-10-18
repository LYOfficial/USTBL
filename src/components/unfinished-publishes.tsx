import { Button, HStack, Text, VStack } from "@chakra-ui/react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { OptionItemGroup } from "@/components/common/option-item";
import { useToast } from "@/contexts/toast";
import { CampusUnfinishedPublish } from "@/models/campus-modpack";
import { CampusModpackService } from "@/services/campus-modpack";

/**
 * Local publish records that are still being monitored or need user action.
 */
export const UnfinishedPublishes: React.FC<{ refreshKey: number }> = ({
  refreshKey,
}) => {
  const { t } = useTranslation();
  const toast = useToast();
  const [jobs, setJobs] = useState<CampusUnfinishedPublish[]>([]);

  const refresh = useCallback(() => {
    CampusModpackService.listCampusModpackPublishJobs().then((response) => {
      if (response.status === "success") setJobs(response.data);
    });
  }, []);

  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, 3000);
    return () => clearInterval(timer);
  }, [refresh, refreshKey]);

  // A publish that just failed becomes an entry here.
  useEffect(() => CampusModpackService.onPublishFinished(refresh), [refresh]);

  const run = useCallback(
    async (
      action: (jobId: string) => Promise<{
        status: string;
        message?: string;
        details?: string;
      }>,
      jobId: string
    ) => {
      const response = await action(jobId);
      if (response.status !== "success") {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
      refresh();
    },
    [refresh, toast]
  );

  const visible = jobs;
  if (visible.length === 0) return null;

  return (
    <VStack align="stretch" spacing={2}>
      <Text fontSize="xs-sm" fontWeight="bold">
        {t("DownloadTasksPage.unfinishedPublish.title")}
      </Text>
      <OptionItemGroup
        items={visible.map((job) => (
          <HStack key={job.jobId} justify="space-between" w="100%">
            <VStack align="start" spacing={0}>
              <Text fontSize="xs-sm">
                {job.instanceName} {job.versionNumber}
              </Text>
              <Text
                fontSize="xs"
                className={job.error ? undefined : "secondary-text"}
                color={job.error ? "red.600" : undefined}
              >
                {job.error ??
                  t(`DownloadTasksPage.unfinishedPublish.phase.${job.phase}`)}
              </Text>
            </VStack>
            <HStack>
              <Button
                size="xs"
                variant="ghost"
                onClick={() =>
                  run(
                    CampusModpackService.resumeCampusModpackPublish,
                    job.jobId
                  )
                }
                isDisabled={job.running}
              >
                {t("DownloadTasksPage.unfinishedPublish.resume")}
              </Button>
              <Button
                size="xs"
                variant="ghost"
                colorScheme="red"
                onClick={() =>
                  run(
                    CampusModpackService.discardCampusModpackPublish,
                    job.jobId
                  )
                }
                isDisabled={job.running}
              >
                {t("DownloadTasksPage.unfinishedPublish.discard")}
              </Button>
            </HStack>
          </HStack>
        ))}
      />
    </VStack>
  );
};
