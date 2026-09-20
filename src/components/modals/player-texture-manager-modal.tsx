import {
  Badge,
  Box,
  Button,
  FormControl,
  FormLabel,
  Grid,
  HStack,
  Input,
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
import { useEffect, useMemo, useRef, useState } from "react";
import SegmentedControl from "@/components/common/segmented";
import SkinPreview from "@/components/skin-preview";
import VskinTextureThumbnail from "@/components/vskin-texture-thumbnail";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { PlayerType, SkinModel, TextureType } from "@/enums/account";
import { Player } from "@/models/account";
import { VustbAccount, VustbTexture } from "@/models/vustb";
import { AccountService } from "@/services/account";
import { OutfitDraft, outfitPreview } from "@/utils/outfit-draft";
import { PlayerCreationSource } from "@/utils/player-creation";
import { base64ImgSrc } from "@/utils/string";

const USTB_AUTH_SERVER_URL = "https://www.ustb.world/skinapi/";
const WARDROBE_PAGE_SIZE = 6;

interface PlayerTextureManagerModalProps extends Omit<ModalProps, "children"> {
  player?: Player;
  account?: VustbAccount | null;
  onCreated?: () => void;
  creationSource?: PlayerCreationSource;
}

const PlayerTextureManagerModal: React.FC<PlayerTextureManagerModalProps> = ({
  player,
  account,
  onCreated,
  creationSource,
  isOpen,
  onClose,
  ...modalProps
}) => {
  const toast = useToast();
  const { getPlayerList } = useGlobalData();
  const [type, setType] = useState<"skin" | "cape">("skin");
  const [wardrobe, setWardrobe] = useState<VustbTexture[]>([]);
  const [draft, setDraft] = useState<OutfitDraft>({});
  const [savedDraft, setSavedDraft] = useState<OutfitDraft>({});
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const savingRef = useRef(false);
  const [isCapeVisible, setIsCapeVisible] = useState(true);
  const [wardrobePage, setWardrobePage] = useState(1);
  const previewContainerRef = useRef<HTMLDivElement>(null);
  const [previewWidth, setPreviewWidth] = useState(430);
  const [previewHeight, setPreviewHeight] = useState(360);
  const [wardrobeHeight, setWardrobeHeight] = useState(430);

  const isCreating = !player;
  const isOfflineCreation = isCreating && creationSource === "offline";
  const isVustb =
    (isCreating && creationSource === "vustb") ||
    (player?.playerType === PlayerType.ThirdParty &&
      player.authServer?.authUrl === USTB_AUTH_SERVER_URL);
  const supported =
    isOfflineCreation ||
    player?.playerType === PlayerType.Offline ||
    player?.playerType === PlayerType.Microsoft ||
    isVustb;
  const typeSupported =
    supported &&
    !(player?.playerType === PlayerType.Microsoft && type === "cape");

  const currentSkin = player?.textures.find(
    (item) => item.textureType === "SKIN"
  );
  const currentCape = player?.textures.find(
    (item) => item.textureType === "CAPE"
  );

  useEffect(() => {
    if (isOpen) {
      setIsCapeVisible(true);
      setDraft({});
      setSavedDraft({});
      setName("");
      setError("");
      setType("skin");
      setWardrobePage(1);
    }
  }, [isOpen, player?.id, creationSource]);

  const accountSubject = account?.subject;
  useEffect(() => {
    if (!isOpen || !supported) return;
    setWardrobe([]);
    setIsLoading(false);
    if (isCreating && isVustb && !accountSubject) return;
    let cancelled = false;
    setIsLoading(true);
    setWardrobe([]);
    AccountService.retrieveVustbWardrobe(
      undefined,
      isOfflineCreation && !accountSubject
    ).then((response) => {
      if (cancelled) return;
      if (response.status === "success") {
        setWardrobe(
          response.data.filter((item) => !(isVustb && item.localBackup))
        );
      } else {
        setError(response.details || response.message);
      }
      setIsLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, [
    isOpen,
    isVustb,
    supported,
    isCreating,
    isOfflineCreation,
    accountSubject,
  ]);

  const items = useMemo(
    () => wardrobe.filter((item) => item.type === type),
    [wardrobe, type]
  );
  const selected = draft[type];

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
      const height = Math.min(
        430,
        Math.max(220, window.innerHeight - (isCreating ? 270 : 190))
      );
      setWardrobeHeight(height);
      setPreviewHeight(Math.min(360, height - 44));
    };
    updateHeight();
    window.addEventListener("resize", updateHeight);
    return () => window.removeEventListener("resize", updateHeight);
  }, [isOpen, isCreating]);

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
    return outfitPreview(
      { ...savedDraft, ...draft },
      {
        skin: currentSkin && base64ImgSrc(currentSkin.image),
        cape: currentCape && base64ImgSrc(currentCape.image),
        model: currentSkin?.model,
      }
    );
  }, [currentCape, currentSkin, draft, savedDraft]);

  const apply = async () => {
    if (savingRef.current) return;
    savingRef.current = true;
    setIsSaving(true);
    setError("");
    try {
      if (!player && isOfflineCreation) {
        const response = await AccountService.addPlayerOffline(
          name.trim(),
          undefined,
          Object.values(draft).filter((item): item is VustbTexture => !!item)
        );
        if (response.status !== "success") {
          setError(response.details || response.message);
          return;
        }
        getPlayerList(true);
        onClose();
        toast({ title: `离线角色 ${name.trim()} 已创建`, status: "success" });
      } else if (!player && isVustb) {
        const response = await AccountService.createVustbProfile(
          name.trim(),
          draft.skin?.hash,
          draft.cape?.hash
        );
        if (response.status !== "success") {
          setError(response.details || response.message);
          return;
        }
        // Do not confuse a successful paid creation with a subsequent sync failure.
        onClose();
        toast({
          title: `角色 ${response.data.name} 已创建`,
          status: "success",
        });
        const sync = await AccountService.syncVustbAccount();
        getPlayerList(true);
        onCreated?.();
        if (sync.status !== "success") {
          toast({
            title: "角色已创建，账户同步未完成，请稍后同步账户",
            description: sync.details || sync.message,
            status: "warning",
          });
        }
      } else if (player && isVustb) {
        const response = await AccountService.applyVustbOutfit(
          player.id,
          Object.values(draft).filter((item): item is VustbTexture => !!item),
          draft.skin === null,
          draft.cape === null
        );
        if (response.status !== "success") {
          setError(response.details || response.message);
          return;
        }
        getPlayerList(true);
        onClose();
      } else if (player) {
        // Keep the existing local-backup/Microsoft behavior. If the second
        // operation fails, only the remaining draft is offered for retry.
        let applied = false;
        for (const textureType of ["skin", "cape"] as const) {
          const texture = draft[textureType];
          if (texture === undefined) continue;
          const response = texture
            ? await AccountService.applyVustbTextureToPlayer(player.id, texture)
            : await AccountService.clearPlayerTexture(
                player.id,
                textureType === "skin" ? TextureType.Skin : TextureType.Cape
              );
          if (response.status !== "success") {
            setError(
              (applied ? "部分材质已保存，剩余材质未应用：" : "") +
                (response.details || response.message)
            );
            return;
          }
          applied = true;
          setSavedDraft((value) => ({ ...value, [textureType]: texture }));
          setDraft((value) => {
            const next = { ...value };
            delete next[textureType];
            return next;
          });
          getPlayerList(true);
        }
        onClose();
      }
    } finally {
      setIsSaving(false);
      savingRef.current = false;
    }
  };

  const creationDisabled =
    !/^[A-Za-z0-9_]{1,16}$/.test(name.trim()) ||
    (!isOfflineCreation &&
      (!account || account.pixelPoints < 5 || account.profiles.length >= 10));

  return (
    <Modal
      isOpen={isOpen}
      onClose={() => {
        if (!savingRef.current) onClose();
      }}
      size="4xl"
      {...modalProps}
    >
      <ModalOverlay />
      <ModalContent
        width={{
          base: "calc(100vw - 2rem)",
          md: "min(900px, calc(100vw - 4rem))",
        }}
        maxW="900px"
        maxH="calc(100vh - 2rem)"
      >
        <ModalHeader>
          {isCreating
            ? `创建${isOfflineCreation ? "离线" : "像素北科"}角色`
            : `更换 ${player.name} 的皮肤与披风`}
        </ModalHeader>
        <ModalCloseButton isDisabled={isSaving} />
        <ModalBody overflowY="auto" minH={0}>
          {isCreating && (
            <FormControl
              mb={3}
              isRequired
              isDisabled={isSaving || (isVustb && !account)}
            >
              <FormLabel fontSize="sm">角色名称</FormLabel>
              <Input
                size="sm"
                value={name}
                maxLength={16}
                placeholder="1–16 位字母、数字或下划线"
                onChange={(event) => setName(event.target.value)}
              />
              <Text mt={1} fontSize="xs" color="gray.500">
                {isOfflineCreation
                  ? "仅保存在本地，不消耗积分；未登录像素北科时仅显示本地衣柜。"
                  : account
                    ? `创建消耗 5 像素积分 · 当前 ${account.pixelPoints ?? 0} 积分 · ${account.profiles.length}/10 个角色`
                    : "请先在账户页面登录像素北科账户"}
              </Text>
            </FormControl>
          )}
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
                  skinSrc={preview.skin || "/images/skins/steve.png"}
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
              <VStack align="stretch" minH={0} h={`${wardrobeHeight}px`}>
                <SegmentedControl
                  size="sm"
                  selected={type}
                  items={[
                    { value: "skin", label: "皮肤" },
                    { value: "cape", label: "披风" },
                  ]}
                  onSelectItem={(value) => {
                    if (isSaving) return;
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
                    flex="1"
                    minH={0}
                    templateRows="repeat(2, minmax(0, 1fr))"
                    overflow="hidden"
                  >
                    {visibleItems.map((item) => (
                      <Box
                        key={item.hash}
                        as="button"
                        type="button"
                        textAlign="left"
                        disabled={isSaving}
                        aria-label={`选择${item.type === "skin" ? "皮肤" : "披风"} ${item.name}`}
                        aria-pressed={selected?.hash === item.hash}
                        borderWidth="2px"
                        borderColor={
                          selected?.hash === item.hash
                            ? "blue.400"
                            : "transparent"
                        }
                        rounded="md"
                        overflow="hidden"
                        display="flex"
                        flexDirection="column"
                        minH={0}
                        cursor="pointer"
                        onClick={() =>
                          setDraft((value) => ({ ...value, [type]: item }))
                        }
                      >
                        <VskinTextureThumbnail
                          texture={item}
                          w="100%"
                          flex="1"
                          minH={0}
                        />
                        <HStack
                          p={2}
                          spacing={1}
                          minW={0}
                          flexShrink={0}
                          w="100%"
                        >
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
                {!isLoading && (
                  <HStack
                    justify="center"
                    spacing={2}
                    visibility={wardrobePageCount > 1 ? "visible" : "hidden"}
                  >
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
          <Text role="status" fontSize="xs" mt={2}>
            皮肤：
            {draft.skin === undefined
              ? isCreating
                ? "默认皮肤"
                : "当前皮肤"
              : draft.skin?.name || "默认皮肤"}
            {" · "}披风：
            {draft.cape === undefined
              ? isCreating
                ? "无"
                : "当前披风"
              : draft.cape?.name || "无"}
          </Text>
          {error && (
            <Text role="alert" color="red.500" fontSize="sm" mt={2}>
              {error}
            </Text>
          )}
        </ModalBody>
        <ModalFooter gap={2}>
          <Button
            variant="ghost"
            onClick={() => setDraft((value) => ({ ...value, [type]: null }))}
            isDisabled={isSaving || !typeSupported}
          >
            {type === "skin" ? "恢复默认皮肤" : "清除披风"}
          </Button>
          <Button
            colorScheme="blue"
            onClick={apply}
            isLoading={isSaving}
            isDisabled={
              !supported ||
              (isCreating ? creationDisabled : Object.keys(draft).length === 0)
            }
          >
            {isCreating
              ? isOfflineCreation
                ? "创建离线角色"
                : "创建角色 · 5 积分"
              : "应用搭配"}
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default PlayerTextureManagerModal;
