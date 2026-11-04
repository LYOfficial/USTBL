import {
  Button,
  Input,
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
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";
import { useRenameInstance } from "@/hooks/rename-instance";

interface RenameSuggestionModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
  version: string;
  suggestedName: string;
  /** Called once the dialog is closed, whatever the answer. */
  onDone: () => void;
}

/** Offers to rename an instance after its modpack updated to a new version. */
const RenameSuggestionModal: React.FC<RenameSuggestionModalProps> = ({
  instanceId,
  version,
  suggestedName,
  onDone,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const rename = useRenameInstance();
  const [name, setName] = useState(suggestedName);
  const [isRenaming, setIsRenaming] = useState(false);

  const close = () => {
    onDone();
    modalProps.onClose();
  };

  const submit = async () => {
    const next = name.trim();
    if (!next) return;
    setIsRenaming(true);
    const shown = await rename(instanceId, next);
    setIsRenaming(false);
    if (shown !== null) close();
  };

  return (
    <Modal
      isCentered
      size="lg"
      closeOnEsc={!isRenaming}
      closeOnOverlayClick={false}
      {...modalProps}
      onClose={close}
    >
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>{t("RenameSuggestionModal.title")}</ModalHeader>
        <ModalCloseButton isDisabled={isRenaming} />
        <ModalBody>
          <VStack align="stretch" spacing={3}>
            <Text fontSize="sm">
              {t("RenameSuggestionModal.body", { version })}
            </Text>
            <Input
              size="sm"
              value={name}
              focusBorderColor={`${primaryColor}.500`}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void submit();
              }}
            />
          </VStack>
        </ModalBody>
        <ModalFooter gap={2}>
          <Button variant="ghost" onClick={close} isDisabled={isRenaming}>
            {t("RenameSuggestionModal.button.keep")}
          </Button>
          <Button
            colorScheme={primaryColor}
            onClick={() => void submit()}
            isLoading={isRenaming}
            isDisabled={name.trim() === ""}
          >
            {t("RenameSuggestionModal.button.rename")}
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default RenameSuggestionModal;
