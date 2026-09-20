import {
  Box,
  BoxProps,
  Flex,
  HStack,
  Icon,
  IconButton,
  Popover,
  PopoverBody,
  PopoverContent,
  PopoverTrigger,
  Switch,
  Text,
  Tooltip,
  VStack,
} from "@chakra-ui/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { BsPersonRaisedHand } from "react-icons/bs";
import {
  FaCircleCheck,
  FaPerson,
  FaPersonFalling,
  FaPersonRunning,
  FaPersonWalking,
  FaRegCircle,
  FaRegImage,
} from "react-icons/fa6";
import {
  LuCircleX,
  LuPause,
  LuPlay,
  LuRefreshCw,
  LuRefreshCwOff,
} from "react-icons/lu";
import * as skinview3d from "skinview3d";
import { useLauncherConfig } from "@/contexts/config";
import { SkinModel } from "@/enums/account";
import { SpringAnimation } from "@/utils/skin-animation";
import { createSkinPreviewResizer } from "@/utils/skin-preview-resize";
import {
  configureEnhancedSkinRendering,
  refreshEnhancedSkinMaterials,
} from "@/utils/skin-rendering";

type AnimationType = "idle" | "walk" | "run" | "wave" | "spring";
type backgroundType = "none" | "black" | "panorama";
type ControlBarVariant = "default" | "overlay";

interface SkinPreviewProps extends Omit<BoxProps, "width" | "height"> {
  skinSrc?: string;
  capeSrc?: string;
  width?: number;
  height?: number;
  animation?: AnimationType;
  canvasBg?: backgroundType;
  isCapeVisible?: boolean;
  onCapeVisibilityChange?: (show: boolean) => void;
  errorMessage?: string | null;
  onSkinError?: (msg: string | null) => void;
  showControlBar?: boolean;
  controlBarVariant?: ControlBarVariant;
  playEntranceAnimation?: boolean;
  enhancedRendering?: boolean;
  skinModel?: SkinModel;
}

const SkinPreview: React.FC<SkinPreviewProps> = ({
  skinSrc,
  capeSrc,
  width = 300,
  height = 400,
  animation = "walk",
  canvasBg = "none",
  isCapeVisible = true,
  onCapeVisibilityChange,
  errorMessage,
  onSkinError,
  showControlBar = true,
  controlBarVariant = "default",
  playEntranceAnimation = false,
  enhancedRendering = false,
  skinModel,
  ...props
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const skinViewerRef = useRef<skinview3d.SkinViewer | null>(null);
  const renderingCleanupRef = useRef<(() => void) | null>(null);
  const resizerRef = useRef<ReturnType<typeof createSkinPreviewResizer> | null>(
    null
  );
  const canvasHeight = Math.max(
    1,
    controlBarVariant === "overlay" ? height : height - 40
  );
  const sizeRef = useRef({ width, height: canvasHeight });
  const entrancePlayedRef = useRef(false);
  const [currentAnimation, setCurrentAnimation] =
    useState<AnimationType>(animation);
  const [background, setBackground] = useState<backgroundType>(canvasBg);
  const [autoRotate, setAutoRotate] = useState(false);
  const [isPlaying, setIsPlaying] = useState(true);

  useEffect(() => {
    sizeRef.current = { width, height: canvasHeight };
  }, [width, canvasHeight]);

  // animation
  const animationList = useMemo(
    () => ({
      idle: {
        icon: <FaPerson />,
        createAnimation: () => new skinview3d.IdleAnimation(),
      },
      walk: {
        icon: <FaPersonWalking />,
        createAnimation: () => new skinview3d.WalkingAnimation(),
      },
      run: {
        icon: <FaPersonRunning />,
        createAnimation: () => new skinview3d.RunningAnimation(),
      },
      wave: {
        icon: <BsPersonRaisedHand />,
        createAnimation: () => new skinview3d.WaveAnimation(),
      },
      spring: {
        icon: <FaPersonFalling />,
        createAnimation: (onComplete?: () => void) =>
          new SpringAnimation(onComplete),
      },
    }),
    []
  );

  const animationTypes = Object.keys(animationList) as AnimationType[];

  const playSpringAnimation = useCallback(() => {
    const viewer = skinViewerRef.current;
    if (!viewer || entrancePlayedRef.current) return;

    entrancePlayedRef.current = true;
    viewer.animation = animationList.spring.createAnimation(() => {
      if (skinViewerRef.current === viewer) {
        viewer.animation = new skinview3d.IdleAnimation();
      }
    });
  }, [animationList]);

  const initSkinViewer = useCallback(() => {
    if (!canvasRef.current) return;
    resizerRef.current?.dispose();
    renderingCleanupRef.current?.();
    renderingCleanupRef.current = null;
    if (skinViewerRef.current) skinViewerRef.current.dispose();
    skinViewerRef.current = new skinview3d.SkinViewer({
      canvas: canvasRef.current,
      width: Math.max(1, sizeRef.current.width),
      height: sizeRef.current.height,
      pixelRatio: enhancedRendering
        ? Math.min(window.devicePixelRatio, 1.5)
        : undefined,
    });

    skinViewerRef.current.zoom = 0.8;
    skinViewerRef.current.controls.enableZoom = false;
    resizerRef.current = createSkinPreviewResizer(skinViewerRef.current);
    if (enhancedRendering) {
      renderingCleanupRef.current = configureEnhancedSkinRendering(
        skinViewerRef.current
      );
    }
  }, [enhancedRendering]);

  useEffect(() => {
    initSkinViewer();
    return () => {
      resizerRef.current?.dispose();
      resizerRef.current = null;
      renderingCleanupRef.current?.();
      renderingCleanupRef.current = null;
      skinViewerRef.current?.dispose();
      skinViewerRef.current = null;
    };
  }, [initSkinViewer]);

  useEffect(() => {
    resizerRef.current?.resize(width, canvasHeight);
  }, [width, canvasHeight, initSkinViewer]);

  useEffect(() => {
    const viewer = skinViewerRef.current;
    if (!viewer) return;

    viewer.autoRotate = isPlaying && autoRotate;
    if (!isPlaying) {
      viewer.animation = null;
    } else if (currentAnimation === "spring") {
      viewer.animation = animationList.spring.createAnimation(() => {
        setCurrentAnimation("idle");
      });
    } else {
      viewer.animation = animationList[currentAnimation].createAnimation();
    }
    if (!isPlaying) setAutoRotate(false);
  }, [animationList, autoRotate, currentAnimation, isPlaying, initSkinViewer]);

  useEffect(() => {
    onCapeVisibilityChange?.(isCapeVisible);
  }, [onCapeVisibilityChange, isCapeVisible]);

  useEffect(() => {
    const viewer = skinViewerRef.current;
    if (!viewer || !skinSrc) return;
    let cancelled = false;
    (async () => {
      try {
        await viewer.loadSkin(skinSrc, {
          model: skinModel
            ? skinModel === SkinModel.Slim
              ? "slim"
              : "default"
            : "auto-detect",
        });
        if (cancelled || skinViewerRef.current !== viewer) return;
        if (isCapeVisible && capeSrc) {
          await viewer.loadCape(capeSrc);
        } else {
          viewer.resetCape();
        }
        if (cancelled || skinViewerRef.current !== viewer) return;
        if (enhancedRendering) {
          refreshEnhancedSkinMaterials(viewer);
        }
        onSkinError?.(null);
        if (playEntranceAnimation) {
          playSpringAnimation();
        }
      } catch (error) {
        if (cancelled || skinViewerRef.current !== viewer) return;
        const errorMsg =
          error instanceof Error
            ? error.message
            : t("SkinPreview.error.loadSkin");
        onSkinError?.(errorMsg);
        logger.error(`SkinPreview error: ${errorMsg}`);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [
    skinViewerRef,
    skinSrc,
    capeSrc,
    isCapeVisible,
    t,
    initSkinViewer,
    skinModel,
    onSkinError,
    playEntranceAnimation,
    playSpringAnimation,
    enhancedRendering,
  ]);

  // background
  const backgroundList = useMemo(
    () => ({
      none: {
        colorScheme: "black",
        btnVariant: "outline",
        operation: () => {
          if (skinViewerRef.current) skinViewerRef.current.background = null;
        },
      },
      black: {
        colorScheme: "gray",
        btnVariant: "solid",
        operation: () => {
          if (skinViewerRef.current)
            skinViewerRef.current.background = "#2D3748";
        },
      },
      panorama: {
        bg: "/images/skins/panorama.jpg",
        colorScheme: "blackAlpha",
        btnVariant: "solid",
        operation: () => {
          if (skinViewerRef.current)
            skinViewerRef.current.loadPanorama("/images/skins/panorama.jpg");
        },
      },
    }),
    [skinViewerRef]
  );

  const backgroundTypes = Object.keys(backgroundList) as backgroundType[];

  const overlayButtonProps =
    controlBarVariant === "overlay"
      ? {
          size: "xs",
          bg: "whiteAlpha.900",
          color: "gray.800",
          _hover: { bg: "white" },
          _active: { bg: "gray.100" },
          boxShadow: "sm",
        }
      : {};

  const BackGroundSelector = () => {
    return (
      <Popover placement="top-start">
        <PopoverTrigger>
          <IconButton
            variant="ghost"
            aria-label="切换预览背景"
            icon={<FaRegImage />}
            {...overlayButtonProps}
          />
        </PopoverTrigger>
        <PopoverContent width="auto" maxWidth="none">
          <PopoverBody>
            <HStack>
              {backgroundTypes.map((type) => (
                <IconButton
                  key={type}
                  size="xs"
                  colorScheme={backgroundList[type].colorScheme}
                  variant={backgroundList[type].btnVariant}
                  aria-label="color"
                  icon={
                    type === background ? <FaCircleCheck /> : <FaRegCircle />
                  }
                  style={
                    type === "panorama"
                      ? {
                          backgroundImage: `url(${backgroundList["panorama"].bg})`,
                          backgroundSize: "cover",
                          backgroundPosition: "center",
                        }
                      : {}
                  }
                  onClick={() => {
                    setBackground(type);
                    backgroundList[type].operation();
                  }}
                />
              ))}
            </HStack>
          </PopoverBody>
        </PopoverContent>
      </Popover>
    );
  };

  useEffect(() => {
    backgroundList[background].operation();
  }, [background, backgroundList, initSkinViewer]);

  return (
    <VStack
      {...props}
      width={width}
      height={height}
      spacing={0}
      position="relative"
      overflow="hidden"
    >
      {errorMessage && (
        <VStack
          width={width}
          height={canvasHeight}
          justifyContent="center"
          spacing={4}
        >
          <Icon as={LuCircleX} boxSize={12} color="red.500" />
          <Text className="secondary-text">{errorMessage}</Text>
        </VStack>
      )}
      <Box
        width="100%"
        height={canvasHeight}
        flexShrink={0}
        display={errorMessage ? "none" : "flex"}
        justifyContent="center"
        overflow="hidden"
      >
        <canvas
          ref={canvasRef}
          style={{
            display: "block",
            width: "auto",
            height: "100%",
            maxWidth: "none",
            flexShrink: 0,
          }}
        />
      </Box>
      {showControlBar && (
        <Flex
          alignItems="center"
          justifyContent="space-between"
          {...(controlBarVariant === "overlay"
            ? {
                position: "absolute",
                left: 2,
                right: 8,
                bottom: 2,
                zIndex: 2,
                width: "auto",
              }
            : { mt: 2, width: "100%" })}
        >
          <HStack spacing={0}>
            <BackGroundSelector />
            <Tooltip label={t(`SkinPreview.animation.${currentAnimation}`)}>
              <IconButton
                aria-label="Switch Animation"
                icon={animationList[currentAnimation].icon}
                variant="ghost"
                {...overlayButtonProps}
                onClick={() => {
                  const currentIndex = animationTypes.indexOf(currentAnimation);
                  const nextIndex = (currentIndex + 1) % animationTypes.length;
                  setCurrentAnimation(animationTypes[nextIndex]);
                }}
              />
            </Tooltip>
            <Tooltip
              label={t(
                `SkinPreview.button.${autoRotate ? "disable" : "enable"}Rotation`
              )}
            >
              <IconButton
                aria-label="Toggle Rotation"
                icon={autoRotate ? <LuRefreshCw /> : <LuRefreshCwOff />}
                variant="ghost"
                {...overlayButtonProps}
                onClick={() => setAutoRotate(!autoRotate)}
              />
            </Tooltip>
            <Tooltip
              label={t(`SkinPreview.button.${isPlaying ? "pause" : "play"}`)}
            >
              <IconButton
                aria-label="Play/Pause Animation"
                icon={isPlaying ? <LuPause /> : <LuPlay />}
                variant="ghost"
                {...overlayButtonProps}
                onClick={() => {
                  setIsPlaying(!isPlaying);
                  if (isPlaying) {
                    setAutoRotate(false);
                  }
                }}
              />
            </Tooltip>
          </HStack>
          {controlBarVariant === "default" && (
            <HStack>
              <Text fontSize="sm">{t("SkinPreview.cape")}</Text>
              <Switch
                isChecked={isCapeVisible}
                onChange={(e) => onCapeVisibilityChange?.(e.target.checked)}
                colorScheme={primaryColor}
              />
            </HStack>
          )}
        </Flex>
      )}
    </VStack>
  );
};

export default SkinPreview;
