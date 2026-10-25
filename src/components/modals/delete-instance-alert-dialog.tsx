import {
  AlertDialogProps,
  Modal,
  ModalBody,
  ModalContent,
  ModalHeader,
  ModalOverlay,
  Progress,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import GenericConfirmDialog from "@/components/modals/generic-confirm-dialog";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import {
  InstanceDeletionProgress,
  InstanceSummary,
} from "@/models/instance/misc";
import { InstanceService } from "@/services/instance";

interface DeleteInstanceDialogProps extends Omit<AlertDialogProps, "children"> {
  instance: InstanceSummary;
}

// Make it a separate component for use with the shared-modal-provider (and context).
const DeleteInstanceDialog: React.FC<DeleteInstanceDialogProps> = ({
  instance,
  ...dialogProps
}) => {
  const { t } = useTranslation();
  const toast = useToast();
  const router = useRouter();
  const { getInstanceList } = useGlobalData();
  const { config } = useLauncherConfig();
  const deleteStarted = useRef(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [progress, setProgress] = useState<InstanceDeletionProgress>({
    stage: "scanning",
    totalFiles: 0,
    deletedFiles: 0,
  });
  const { onClose, isOpen } = dialogProps;
  const skipConfirmation =
    config.suppressedDialogs?.includes("deleteInstanceAlert") ?? false;

  const handleDeleteInstance = useCallback(async () => {
    if (deleteStarted.current) return;
    deleteStarted.current = true;
    setIsDeleting(true);
    try {
      const response = await InstanceService.deleteInstance(
        instance.id,
        setProgress
      );
      getInstanceList(true);
      if (response.status === "success") {
        toast({
          title: response.message,
          status: "success",
        });
        if (!router.asPath.startsWith("/instances/list")) {
          await router.push("/instances/list");
        }
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    } catch (error) {
      toast({
        title: t("Services.instance.deleteInstance.error.title"),
        description: String(error),
        status: "error",
      });
    } finally {
      onClose();
    }
  }, [getInstanceList, instance.id, onClose, router, t, toast]);

  useEffect(() => {
    if (isOpen && skipConfirmation) {
      void handleDeleteInstance();
    }
  }, [handleDeleteInstance, isOpen, skipConfirmation]);

  return (
    <>
      <GenericConfirmDialog
        isOpen={isOpen && !isDeleting && !skipConfirmation}
        onClose={() => {
          if (!deleteStarted.current) onClose();
        }}
        title={t("DeleteInstanceAlertDialog.dialog.title")}
        body={
          <VStack align="stretch">
            <Text>
              {t("DeleteInstanceAlertDialog.dialog.content", {
                instanceName: instance.name,
              })}
            </Text>
            <Text>
              {t(
                `DeleteInstanceAlertDialog.dialog.warning.${instance.isVersionIsolated ? "withVerIso" : "woVerIso"}`
              )}
            </Text>
          </VStack>
        }
        btnOK={t("General.delete")}
        onOKCallback={() => {
          void handleDeleteInstance();
        }}
        isAlert
        showSuppressBtn
        suppressKey="deleteInstanceAlert"
      />
      <Modal
        isOpen={isDeleting}
        onClose={() => {}}
        closeOnOverlayClick={false}
        closeOnEsc={false}
        isCentered
      >
        <ModalOverlay />
        <ModalContent>
          <ModalHeader>
            {t("DeleteInstanceAlertDialog.progress.title")}
          </ModalHeader>
          <ModalBody pb={6}>
            <VStack align="stretch" spacing={4}>
              <Text wordBreak="break-word">{instance.name}</Text>
              <Text role="status" aria-live="polite">
                {t(`DeleteInstanceAlertDialog.progress.${progress.stage}`)}
              </Text>
              <Progress
                colorScheme={config.appearance.theme.primaryColor}
                hasStripe
                isAnimated
                isIndeterminate={
                  progress.stage === "scanning" || progress.totalFiles === 0
                }
                value={
                  progress.totalFiles > 0
                    ? (progress.deletedFiles / progress.totalFiles) * 100
                    : 0
                }
                aria-label={t("DeleteInstanceAlertDialog.progress.title")}
                borderRadius="md"
              />
              <Text fontSize="sm" textAlign="center">
                {t("DeleteInstanceAlertDialog.progress.count", {
                  deleted: progress.deletedFiles,
                  total: progress.totalFiles,
                })}
              </Text>
            </VStack>
          </ModalBody>
        </ModalContent>
      </Modal>
    </>
  );
};

export default DeleteInstanceDialog;
