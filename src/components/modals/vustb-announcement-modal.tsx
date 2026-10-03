import { Modal, ModalBody, ModalCloseButton, ModalContent, ModalHeader, ModalOverlay, ModalProps } from "@chakra-ui/react";
import type React from "react";
import MarkdownContainer from "@/components/common/markdown-container";
import { VustbAnnouncement } from "@/models/vustb";

interface Props extends Omit<ModalProps, "children"> {
  announcement: VustbAnnouncement;
}

const VustbAnnouncementModal: React.FC<Props> = ({ announcement, ...props }) => (
  <Modal size="xl" scrollBehavior="inside" {...props}>
    <ModalOverlay />
    <ModalContent>
      <ModalHeader>{announcement.title}</ModalHeader>
      <ModalCloseButton />
      <ModalBody pb={6}><MarkdownContainer>{announcement.content}</MarkdownContainer></ModalBody>
    </ModalContent>
  </Modal>
);

export default VustbAnnouncementModal;
