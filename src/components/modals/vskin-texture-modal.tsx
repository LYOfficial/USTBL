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
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { PlayerType, SkinModel } from "@/enums/account";
import { Player } from "@/models/account";
import { VustbTexture } from "@/models/vustb";
import { AccountService } from "@/services/account";

const USTB_AUTH_SERVER_URL = "https://www.ustb.world/skinapi/";

export const canApplyVskinTexture = (
  player: Player | undefined,
  texture: VustbTexture
) =>
  !!player &&
  (player.playerType === PlayerType.Offline ||
    (player.playerType === PlayerType.ThirdParty &&
      player.authServer?.authUrl === USTB_AUTH_SERVER_URL) ||
    (player.playerType === PlayerType.Microsoft && texture.type === "skin"));

interface VskinTextureModalProps extends Omit<ModalProps, "children"> {
  texture?: VustbTexture;
  player?: Player;
  onCollected?: (hash: string) => void;
}

const VskinTextureModal: React.FC<VskinTextureModalProps> = ({
  texture,
  player,
  onCollected,
  isOpen,
  onClose,
  ...modalProps
}) => {
  const toast = useToast();
  const { getPlayerList } = useGlobalData();
  const [isCollecting, setIsCollecting] = useState(false);
  const [isApplying, setIsApplying] = useState(false);
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

  const apply = async () => {
    if (!player) return;
    setIsApplying(true);
    if (!texture.collected) {
      const collected = await AccountService.collectVustbTexture(texture.hash);
      if (collected.status !== "success") {
        toast({
          title: collected.details || collected.message,
          status: "error",
        });
        setIsApplying(false);
        return;
      }
      onCollected?.(texture.hash);
    }
    const response = await AccountService.applyVustbTextureToPlayer(
      player.id,
      texture
    );
    if (response.status === "success") {
      getPlayerList(true);
      toast({ title: `已应用到 ${player.name}`, status: "success" });
      onClose();
    } else {
      toast({
        title: response.details || response.message,
        status: "error",
      });
    }
    setIsApplying(false);
  };

  const canApply = canApplyVskinTexture(player, texture);
  const unavailableReason = !player
    ? "请先选择角色"
    : player.playerType === PlayerType.Microsoft && texture.type === "cape"
      ? "微软账户不能上传自定义披风"
      : "该角色类型不支持从 vSkin 更换材质";

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
            <Button
              colorScheme="blue"
              onClick={apply}
              isLoading={isApplying}
              isDisabled={!canApply}
              title={canApply ? undefined : unavailableReason}
            >
              应用到当前角色
            </Button>
          </HStack>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default VskinTextureModal;
