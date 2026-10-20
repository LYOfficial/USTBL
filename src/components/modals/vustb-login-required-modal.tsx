import {
  Button,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  Text,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";

const VustbLoginRequiredModal: React.FC<{
  isOpen: boolean;
  onClose: () => void;
}> = ({ isOpen, onClose }) => {
  const router = useRouter();
  const { t } = useTranslation();
  const { config } = useLauncherConfig();

  const goToAccounts = () => {
    onClose();
    void router.push("/accounts");
  };

  return (
    <Modal isOpen={isOpen} onClose={onClose} isCentered>
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>{t("VustbAccessGate.title")}</ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          <Text>{t("VustbAccessGate.description")}</Text>
        </ModalBody>
        <ModalFooter gap={3}>
          <Button variant="ghost" onClick={onClose}>
            {t("General.cancel")}
          </Button>
          <Button
            colorScheme={config.appearance.theme.primaryColor}
            onClick={goToAccounts}
          >
            {t("VustbAccessGate.goToAccounts")}
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default VustbLoginRequiredModal;
