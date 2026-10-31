import { Button, HStack, Text, VStack } from "@chakra-ui/react";
import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { OptionItemGroup } from "@/components/common/option-item";
import { useTaskContext } from "@/contexts/task";
import { useToast } from "@/contexts/toast";
import { CampusModpackService } from "@/services/campus-modpack";
import { localizeFileError } from "@/utils/file-error";

/**
 * Local publish records that need the user's attention: an interrupted
 * upload to resume or give up, or a failed publish. Anything the platform
 * is still processing shows on the task list instead, never here (§3.2.3).
 * The records live in the task context, which also drives the nav-bar
 * task button; this component only renders them.
 */
export const UnfinishedPublishes: React.FC = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { unfinishedPublishes, refreshUnfinishedPublishes } = useTaskContext();

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
      refreshUnfinishedPublishes();
    },
    [refreshUnfinishedPublishes, toast]
  );

  const visible = unfinishedPublishes;
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
                {(job.error &&
                  (localizeFileError(t, job.error) ?? job.error)) ||
                  t(`DownloadTasksPage.unfinishedPublish.phase.${job.phase}`)}
              </Text>
            </VStack>
            <HStack>
              {job.resumable && (
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={() =>
                    run(
                      CampusModpackService.resumeCampusModpackPublish,
                      job.jobId
                    )
                  }
                >
                  {t("DownloadTasksPage.unfinishedPublish.resume")}
                </Button>
              )}
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
