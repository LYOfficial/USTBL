import {
  Badge,
  Box,
  Button,
  Grid,
  HStack,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  ModalProps,
  Spinner,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import SegmentedControl from "@/components/common/segmented";
import SkinPreview from "@/components/skin-preview";
import VskinTextureThumbnail from "@/components/vskin-texture-thumbnail";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { PlayerType, SkinModel, TextureType } from "@/enums/account";
import { Player } from "@/models/account";
import { VustbTexture } from "@/models/vustb";
import { AccountService } from "@/services/account";
import { base64ImgSrc } from "@/utils/string";

const USTB_AUTH_SERVER_URL = "https://www.ustb.world/skinapi/";
const WARDROBE_PAGE_SIZE = 6;

interface PlayerTextureManagerModalProps extends Omit<ModalProps, "children"> {
  player: Player;
}

const PlayerTextureManagerModal: React.FC<PlayerTextureManagerModalProps> = ({
  player,
  isOpen,
  onClose,
  ...modalProps
}) => {
  const toast = useToast();
  const { getPlayerList } = useGlobalData();
  const [type, setType] = useState<"skin" | "cape">("skin");
  const [items, setItems] = useState<VustbTexture[]>([]);
  const [selected, setSelected] = useState<VustbTexture>();
  const [isLoading, setIsLoading] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [isCapeVisible, setIsCapeVisible] = useState(true);
  const [wardrobePage, setWardrobePage] = useState(1);
  const previewContainerRef = useRef<HTMLDivElement>(null);
  const [previewWidth, setPreviewWidth] = useState(430);
  const [previewHeight, setPreviewHeight] = useState(360);

  const isVustb =
    player.playerType === PlayerType.ThirdParty &&
    player.authServer?.authUrl === USTB_AUTH_SERVER_URL;
  const supported =
    player.playerType === PlayerType.Offline ||
    player.playerType === PlayerType.Microsoft ||
    isVustb;
  const typeSupported =
    supported &&
    !(player.playerType === PlayerType.Microsoft && type === "cape");

  const currentSkin = player.textures.find(
    (item) => item.textureType === "SKIN"
  );
  const currentCape = player.textures.find(
    (item) => item.textureType === "CAPE"
  );

  const load = useCallback(async () => {
    if (!isOpen || !supported) return;
    setIsLoading(true);
    const response = await AccountService.retrieveVustbWardrobe(type);
    setItems(
      response.status === "success"
        ? response.data.filter((item) => !(isVustb && item.localBackup))
        : []
    );
    if (response.status !== "success") {
      toast({
        title: response.details || response.message,
        status: "error",
      });
    }
    setSelected(undefined);
    setWardrobePage(1);
    setIsLoading(false);
  }, [isOpen, isVustb, supported, toast, type]);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (isOpen) {
      setIsCapeVisible(true);
    }
  }, [isOpen, player.id]);

  const wardrobePageCount = Math.max(
    1,
    Math.ceil(items.length / WARDROBE_PAGE_SIZE)
  );
  const visibleItems = items.slice(
    (wardrobePage - 1) * WARDROBE_PAGE_SIZE,
    wardrobePage * WARDROBE_PAGE_SIZE
  );

  useEffect(() => {
    if (!isOpen) return;
    const updateHeight = () => {
      setPreviewHeight(Math.min(360, Math.max(240, window.innerHeight - 280)));
    };
    updateHeight();
    window.addEventListener("resize", updateHeight);
    return () => window.removeEventListener("resize", updateHeight);
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) return;
    const container = previewContainerRef.current;
    if (!container) return;
    const updateWidth = () => {
      setPreviewWidth(
        Math.min(430, Math.floor(container.getBoundingClientRect().width))
      );
    };
    updateWidth();
    const observer = new ResizeObserver(updateWidth);
    observer.observe(container);
    return () => observer.disconnect();
  }, [isOpen]);

  const preview = useMemo(() => {
    if (selected?.type === "skin") {
      return {
        skin: selected.url,
        cape: currentCape && base64ImgSrc(currentCape.image),
        model: selected.model,
      };
    }
    if (selected?.type === "cape") {
      return {
        skin: currentSkin && base64ImgSrc(currentSkin.image),
        cape: selected.url,
        model: currentSkin?.model,
      };
    }
    return {
      skin: currentSkin && base64ImgSrc(currentSkin.image),
      cape: currentCape && base64ImgSrc(currentCape.image),
      model: currentSkin?.model,
    };
  }, [currentCape, currentSkin, selected]);

  const apply = async () => {
    if (!selected) return;
    setIsSaving(true);
    const response = await AccountService.applyVustbTextureToPlayer(
      player.id,
      selected
    );
    if (response.status === "success") {
      getPlayerList(true);
      toast({ title: "角色材质已更新", status: "success" });
      onClose();
    } else {
      toast({
        title: response.details || response.message,
        status: "error",
      });
    }
    setIsSaving(false);
  };

  const clear = async () => {
    setIsSaving(true);
    const response = await AccountService.clearPlayerTexture(
      player.id,
      type === "skin" ? TextureType.Skin : TextureType.Cape
    );
    if (response.status === "success") {
      getPlayerList(true);
      toast({
        title: type === "skin" ? "皮肤已恢复默认" : "披风已清除",
        status: "success",
      });
      onClose();
    } else {
      toast({
        title: response.details || response.message,
        status: "error",
      });
    }
    setIsSaving(false);
  };

  return (
    <Modal isOpen={isOpen} onClose={onClose} size="4xl" {...modalProps}>
      <ModalOverlay />
      <ModalContent
        width={{
          base: "calc(100vw - 2rem)",
          md: "min(900px, calc(100vw - 4rem))",
        }}
        maxW="900px"
        maxH="calc(100vh - 2rem)"
      >
        <ModalHeader>更换 {player.name} 的皮肤与披风</ModalHeader>
        <ModalCloseButton />
        <ModalBody overflowY="auto" minH={0}>
          {!supported ? (
            <Text py={16} textAlign="center" color="gray.500">
              当前第三方认证服务器不支持启动器内更换材质
            </Text>
          ) : (
            <Grid
              templateColumns={{
                base: "minmax(0, 1fr)",
                md: "minmax(300px, 1fr) minmax(280px, 1fr)",
              }}
              gap={{ base: 3, md: 5 }}
            >
              <Box
                ref={previewContainerRef}
                overflow="hidden"
                rounded="lg"
                width="100%"
                display="flex"
                justifyContent="center"
              >
                <SkinPreview
                  skinSrc={preview.skin}
                  capeSrc={preview.cape}
                  skinModel={
                    preview.model === SkinModel.Slim
                      ? SkinModel.Slim
                      : SkinModel.Default
                  }
                  width={previewWidth}
                  height={previewHeight}
                  showControlBar
                  isCapeVisible={isCapeVisible}
                  onCapeVisibilityChange={setIsCapeVisible}
                />
              </Box>
              <VStack align="stretch" minH={0} h="430px">
                <SegmentedControl
                  size="sm"
                  selected={type}
                  items={[
                    { value: "skin", label: "皮肤" },
                    { value: "cape", label: "披风" },
                  ]}
                  onSelectItem={(value) => {
                    setType(value as "skin" | "cape");
                    setWardrobePage(1);
                  }}
                />
                {!typeSupported ? (
                  <Text py={10} textAlign="center" color="gray.500">
                    微软账户不支持上传自定义披风
                  </Text>
                ) : isLoading ? (
                  <Box py={12} textAlign="center">
                    <Spinner />
                  </Box>
                ) : (
                  <Grid
                    templateColumns="repeat(3, minmax(0, 1fr))"
                    gap={2}
                    h="330px"
                    overflow="hidden"
                  >
                    {visibleItems.map((item) => (
                      <Box
                        key={item.hash}
                        borderWidth="2px"
                        borderColor={
                          selected?.hash === item.hash
                            ? "blue.400"
                            : "transparent"
                        }
                        rounded="md"
                        overflow="hidden"
                        cursor="pointer"
                        onClick={() => setSelected(item)}
                      >
                        <VskinTextureThumbnail texture={item} aspectRatio={1} />
                        <HStack p={2} spacing={1} minW={0}>
                          <Text fontSize="xs" noOfLines={1} minW={0}>
                            {item.name || "未命名"}
                          </Text>
                          {item.localBackup && (
                            <Badge colorScheme="orange" flexShrink={0}>
                              备份
                            </Badge>
                          )}
                        </HStack>
                      </Box>
                    ))}
                    {items.length === 0 && (
                      <Text
                        gridColumn="1 / -1"
                        py={10}
                        textAlign="center"
                        color="gray.500"
                      >
                        衣柜中没有{type === "skin" ? "皮肤" : "披风"}
                      </Text>
                    )}
                  </Grid>
                )}
                {!isLoading && wardrobePageCount > 1 && (
                  <HStack justify="center" spacing={2}>
                    <Button
                      size="xs"
                      variant="ghost"
                      isDisabled={wardrobePage <= 1}
                      onClick={() => setWardrobePage((page) => page - 1)}
                    >
                      上一页
                    </Button>
                    <Text fontSize="xs" color="gray.500">
                      {wardrobePage} / {wardrobePageCount}
                    </Text>
                    <Button
                      size="xs"
                      variant="ghost"
                      isDisabled={wardrobePage >= wardrobePageCount}
                      onClick={() => setWardrobePage((page) => page + 1)}
                    >
                      下一页
                    </Button>
                  </HStack>
                )}
                <HStack mt="auto">
                  <Badge>{items.length} 个可选材质</Badge>
                </HStack>
              </VStack>
            </Grid>
          )}
        </ModalBody>
        <ModalFooter gap={2}>
          <Button
            variant="ghost"
            onClick={clear}
            isLoading={isSaving}
            isDisabled={!typeSupported}
          >
            {type === "skin" ? "恢复默认皮肤" : "清除披风"}
          </Button>
          <Button
            colorScheme="blue"
            onClick={apply}
            isLoading={isSaving}
            isDisabled={!typeSupported || !selected}
          >
            应用所选材质
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default PlayerTextureManagerModal;
