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
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { MemorySuggestion } from "@/models/memory-suggestion";
import { MemorySuggestionService } from "@/services/memory-suggestion";
import {
  formatMemoryMb,
  isMemorySuggestionCapped,
} from "@/utils/memory-suggestion";

interface MemorySuggestionModalProps extends Omit<ModalProps, "children"> {
  suggestion: MemorySuggestion;
  /** The player answered; the suggestion is not asked again. */
  onAnswered: (suggestion: MemorySuggestion) => void;
  /** The player closed it without answering; asked again next launch. */
  onDismissed: (suggestion: MemorySuggestion) => void;
}

/**
 * Asks whether to give an instance the memory its modpack recommends. Nothing
 * changes unless the player applies it.
 */
const MemorySuggestionModal: React.FC<MemorySuggestionModalProps> = ({
  suggestion,
  onAnswered,
  onDismissed,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const toast = useToast();
  const { config } = useLauncherConfig();
  const { getInstanceList } = useGlobalData();
  const primaryColor = config.appearance.theme.primaryColor;
  const keepRef = useRef<HTMLButtonElement>(null);
  const [isAnswering, setIsAnswering] = useState(false);

  const dismiss = () => {
    onDismissed(suggestion);
    modalProps.onClose();
  };

  const answer = async (apply: boolean) => {
    setIsAnswering(true);
    const response = await MemorySuggestionService.answerMemorySuggestion(
      suggestion.instanceId,
      suggestion.suggestedMb,
      apply
    );
    setIsAnswering(false);
    if (response.status !== "success") {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
      return;
    }
    // The instance now uses its own settings; let the pages see it.
    if (apply) getInstanceList(true);
    onAnswered(suggestion);
    modalProps.onClose();
  };

  return (
    <Modal
      scrollBehavior="inside"
      size="lg"
      initialFocusRef={keepRef}
      closeOnEsc={!isAnswering}
      closeOnOverlayClick={!isAnswering}
      {...modalProps}
      onClose={dismiss}
    >
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>{t("MemorySuggestionModal.title")}</ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          <VStack align="stretch" spacing={2}>
            <Text fontSize="sm">
              {t("MemorySuggestionModal.body", {
                pack: suggestion.packName,
                suggested: formatMemoryMb(suggestion.suggestedMb),
                current: formatMemoryMb(suggestion.currentMb),
                source: t(
                  `MemorySuggestionModal.source.${suggestion.currentSource}`
                ),
              })}
            </Text>
            {isMemorySuggestionCapped(suggestion) && (
              <Text fontSize="sm">
                {t("MemorySuggestionModal.clamped", {
                  total: formatMemoryMb(suggestion.totalMb),
                  clamped: formatMemoryMb(suggestion.cappedMb),
                })}
              </Text>
            )}
            <Text fontSize="xs-sm" className="secondary-text">
              {t("MemorySuggestionModal.note")}
            </Text>
          </VStack>
        </ModalBody>
        <ModalFooter gap={2}>
          <Button variant="ghost" onClick={dismiss} isDisabled={isAnswering}>
            {t("MemorySuggestionModal.button.later")}
          </Button>
          <Button
            ref={keepRef}
            variant="outline"
            onClick={() => answer(false)}
            isDisabled={isAnswering}
          >
            {t("MemorySuggestionModal.button.keep")}
          </Button>
          <Button
            colorScheme={primaryColor}
            onClick={() => answer(true)}
            isLoading={isAnswering}
          >
            {t("MemorySuggestionModal.button.apply")}
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default MemorySuggestionModal;
