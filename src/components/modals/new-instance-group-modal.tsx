import {
  Button,
  FormControl,
  FormErrorMessage,
  Input,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  ModalProps,
} from "@chakra-ui/react";
import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";

interface NewInstanceGroupModalProps extends Omit<ModalProps, "children"> {
  isNameTaken: (name: string) => boolean;
  onCreate: (name: string) => void;
}

const NewInstanceGroupModal: React.FC<NewInstanceGroupModalProps> = ({
  isNameTaken,
  onCreate,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const { isOpen, onClose } = modalProps;
  const [name, setName] = useState("");

  useEffect(() => {
    if (isOpen) setName("");
  }, [isOpen]);

  const trimmed = name.trim();
  const taken = !!trimmed && isNameTaken(trimmed);
  const canCreate = !!trimmed && !taken;

  const handleCreate = () => {
    if (!canCreate) return;
    onCreate(trimmed);
    onClose();
  };

  return (
    <Modal {...modalProps} isCentered>
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>{t("InstanceGroups.newGroup.title")}</ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          <FormControl isInvalid={taken}>
            <Input
              autoFocus
              value={name}
              onChange={(event) => setName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.nativeEvent.isComposing) {
                  handleCreate();
                }
              }}
              placeholder={t("InstanceGroups.newGroup.placeholder")}
              focusBorderColor={`${primaryColor}.500`}
            />
            <FormErrorMessage>
              {t("InstanceGroups.newGroup.duplicate")}
            </FormErrorMessage>
          </FormControl>
        </ModalBody>
        <ModalFooter>
          <Button variant="ghost" onClick={onClose}>
            {t("General.cancel")}
          </Button>
          <Button
            colorScheme={primaryColor}
            onClick={handleCreate}
            isDisabled={!canCreate}
          >
            {t("InstanceGroups.newGroup.create")}
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default NewInstanceGroupModal;
