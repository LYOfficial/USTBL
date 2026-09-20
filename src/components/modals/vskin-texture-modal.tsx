import {
  Button,
  Flex,
  HStack,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  ModalProps,
  Text,
  useDisclosure,
} from "@chakra-ui/react";
import { useState } from "react";
import SkinPreview from "@/components/skin-preview";
import { useToast } from "@/contexts/toast";
import { SkinModel } from "@/enums/account";
import { VustbTexture } from "@/models/vustb";
import { AccountService } from "@/services/account";

interface VskinTextureModalProps extends Omit<ModalProps, "children"> {
  texture?: VustbTexture;
  onCollected?: (hash: string) => void;
}

const VskinTextureModal: React.FC<VskinTextureModalProps> = ({
  texture,
  onCollected,
  isOpen,
  onClose,
  ...modalProps
}) => {
  const toast = useToast();
  const [isCollecting, setIsCollecting] = useState(false);
  const capeDisclosure = useDisclosure({ defaultIsOpen: true });
  if (!texture) return null;

  const collect = async () => {
    setIsCollecting(true);
    const response = await AccountService.collectVustbTexture(texture.hash);
    if (response.status === "success") {
      onCollected?.(texture.hash);
      toast({ title: "已收藏到衣柜", status: "success" });
    } else {
      toast({
        title: response.details || response.message,
        status: "error",
      });
    }
    setIsCollecting(false);
  };

  return (
    <Modal isOpen={isOpen} onClose={onClose} size="lg" {...modalProps}>
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>{texture.name || "未命名材质"}</ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          <Flex justify="center">
            <SkinPreview
              skinSrc={
                texture.type === "skin"
                  ? texture.url
                  : "/images/skins/steve.png"
              }
              capeSrc={texture.type === "cape" ? texture.url : undefined}
              skinModel={
                texture.model === "slim" ? SkinModel.Slim : SkinModel.Default
              }
              width={480}
              height={350}
              showControlBar
              isCapeVisible={capeDisclosure.isOpen}
              onCapeVisibilityChange={(visible) =>
                visible ? capeDisclosure.onOpen() : capeDisclosure.onClose()
              }
            />
          </Flex>
          <Text mt={3} fontSize="sm" color="gray.500">
            {texture.uploaderName || "未知上传者"} ·{" "}
            {texture.type === "skin" ? texture.model : "披风"}
          </Text>
        </ModalBody>
        <ModalFooter>
          <HStack>
            <Button
              variant="ghost"
              onClick={collect}
              isLoading={isCollecting}
              isDisabled={texture.collected}
            >
              {texture.collected ? "已在衣柜" : "收藏到衣柜"}
            </Button>
          </HStack>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default VskinTextureModal;
