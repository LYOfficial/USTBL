import {
  Button,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  ModalProps,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";

export interface ModpackUpdateNoticeItem {
  instanceId: string;
  instanceName: string;
  currentLabel?: string;
  latestLabel?: string;
}

interface NotifyModpackUpdatesModalProps extends Omit<ModalProps, "children"> {
  updates: ModpackUpdateNoticeItem[];
}

/**
 * The modpack updates found at startup, announced once per new version.
 */
const NotifyModpackUpdatesModal: React.FC<NotifyModpackUpdatesModalProps> = ({
  updates,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const router = useRouter();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;

  const goToSync = () => {
    modalProps.onClose();
    router.push("/instances/sync");
  };

  return (
    <Modal scrollBehavior="inside" size="lg" {...modalProps}>
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>{t("NotifyModpackUpdatesModal.title")}</ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          <VStack align="stretch" spacing={2}>
            {updates.map((item) => (
              <Text key={item.instanceId} fontSize="xs-sm">
                {item.instanceName}: {item.currentLabel ?? "-"} →{" "}
                {item.latestLabel ?? "-"}
              </Text>
            ))}
          </VStack>
        </ModalBody>
        <ModalFooter gap={2}>
          <Button variant="ghost" onClick={modalProps.onClose}>
            {t("NotifyModpackUpdatesModal.button.later")}
          </Button>
          <Button colorScheme={primaryColor} onClick={goToSync}>
            {t("NotifyModpackUpdatesModal.button.goToSync")}
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default NotifyModpackUpdatesModal;
