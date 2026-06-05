import {
  Box,
  Divider,
  HStack,
  Icon,
  IconButton,
  Tab,
  TabList,
  Tabs,
  Text,
  Tooltip,
  useColorModeValue,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { MouseEvent, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { IconType } from "react-icons";
import {
  LuBox,
  LuCircleUserRound,
  LuCompass,
  LuCopy,
  LuMinus,
  LuSettings,
  LuSquare,
  LuX,
  LuZap,
} from "react-icons/lu";
import { DownloadIndicator } from "@/components/download-indicator";
import { TitleShort } from "@/components/logo-title";
import { useLauncherConfig } from "@/contexts/config";
import { useTaskContext } from "@/contexts/task";

interface NavItem {
  icon: IconType;
  label: string;
  path: string;
}

const WindowTitleBar = () => {
  const router = useRouter();
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const { tasks } = useTaskContext();
  const [isMaximized, setIsMaximized] = useState(false);
  const primaryColor = config.appearance.theme.primaryColor;
  const isSimplified = config.appearance.theme.headNavStyle === "simplified";
  const isDownloadIndicatorShown = tasks.length > 0;
  const unselectTabColor = useColorModeValue("gray.600", "gray.400");

  const navList = useMemo<NavItem[]>(
    () => [
      { icon: LuZap, label: "launch", path: "/launch" },
      { icon: LuBox, label: "instances", path: "/instances" },
      { icon: LuCircleUserRound, label: "accounts", path: "/accounts" },
      { icon: LuCompass, label: "discover", path: "/discover" },
      { icon: LuSettings, label: "settings", path: "/settings" },
    ],
    []
  );

  const selectedIndex = navList.findIndex((item) =>
    router.pathname.startsWith(item.path)
  );

  const isTauriRuntime =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

  const getAppWindow = async () => {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    return getCurrentWindow();
  };

  useEffect(() => {
    const setup = async () => {
      if (!isTauriRuntime) {
        return;
      }

      try {
        const appWindow = await getAppWindow();
        setIsMaximized(await appWindow.isMaximized());
      } catch {
        setIsMaximized(false);
      }
    };

    void setup();
  }, [isTauriRuntime]);

  const onTabChange = (index: number) => {
    const target = navList[index];
    if (target) {
      router.push(target.path);
    }
  };

  const onMinimize = async () => {
    if (!isTauriRuntime) {
      return;
    }

    try {
      const appWindow = await getAppWindow();
      await appWindow.minimize();
    } catch {
      // Ignore when running with insufficient window permission.
    }
  };

  const onToggleMaximize = async () => {
    if (!isTauriRuntime) {
      return;
    }

    try {
      const appWindow = await getAppWindow();
      if (await appWindow.isMaximized()) {
        await appWindow.unmaximize();
      } else {
        await appWindow.maximize();
      }
      setIsMaximized(await appWindow.isMaximized());
    } catch {
      // Ignore when running with insufficient window permission.
    }
  };

  const onClose = async () => {
    if (!isTauriRuntime) {
      return;
    }

    try {
      const appWindow = await getAppWindow();
      await appWindow.close();
    } catch {
      // Ignore when running with insufficient window permission.
    }
  };

  const onStartDrag = async () => {
    if (!isTauriRuntime) {
      return;
    }

    try {
      const appWindow = await getAppWindow();
      await appWindow.startDragging();
    } catch {
      // Ignore when running with insufficient window permission.
    }
  };

  const isNoDragTarget = (target: EventTarget | null) => {
    if (!(target instanceof Element)) {
      return false;
    }

    return Boolean(
      target.closest(
        "button, [role='button'], [role='tab'], a, input, select, textarea, [data-no-drag='true']"
      )
    );
  };

  const onDragMouseDown = (event: MouseEvent) => {
    if (event.button !== 0) {
      return;
    }

    if (isNoDragTarget(event.target)) {
      return;
    }

    void onStartDrag();
  };

  const onDragDoubleClick = (event: MouseEvent) => {
    if (isNoDragTarget(event.target)) {
      return;
    }

    void onToggleMaximize();
  };

  return (
    <HStack
      justify="space-between"
      h="44px"
      px={2}
      borderBottomWidth="1px"
      borderColor="blackAlpha.200"
      bg="blackAlpha.200"
      _dark={{ borderColor: "whiteAlpha.200", bg: "whiteAlpha.100" }}
      userSelect="none"
      spacing={2}
      onMouseDown={onDragMouseDown}
      onDoubleClick={onDragDoubleClick}
    >
      <HStack
        px={2}
        h="100%"
        flexShrink={0}
      >
        <TitleShort transform="scale(0.85)" transformOrigin="left center" />
      </HStack>

      <Tabs
        variant="soft-rounded"
        size="sm"
        colorScheme={primaryColor}
        index={selectedIndex}
        onChange={onTabChange}
        flex={1}
        minW={0}
      >
        <TabList h="100%" alignItems="center" borderBottom="none">
          {navList.map((item, index) => (
            <Tooltip
              key={item.path}
              label={t(`HeadNavBar.navList.${item.label}`)}
              placement="bottom"
              isDisabled={!isSimplified || selectedIndex === index}
            >
              <Tab
                h="30px"
                fontWeight={selectedIndex === index ? "600" : "normal"}
                color={selectedIndex === index ? "inherit" : unselectTabColor}
              >
                <HStack spacing={2} id={`head-navbar-tab-${item.label}`}>
                  <Icon as={item.icon} />
                  {(!isSimplified || selectedIndex === index) && (
                    <Text>{t(`HeadNavBar.navList.${item.label}`)}</Text>
                  )}
                </HStack>
              </Tab>
            </Tooltip>
          ))}
        </TabList>
      </Tabs>

      {isDownloadIndicatorShown && (
        <HStack spacing={2} h="100%" px={1.5} flexShrink={0}>
          <Divider
            orientation="vertical"
            h="18px"
            borderColor="var(--chakra-colors-chakra-placeholder-color)"
          />
          <DownloadIndicator />
        </HStack>
      )}

      <HStack h="100%" minW="24px" flexShrink={0}>
        <Box w="100%" h="100%" />
      </HStack>

      <HStack spacing={0.5}>
        <IconButton
          aria-label="minimize"
          size="sm"
          variant="ghost"
          icon={<Icon as={LuMinus} pointerEvents="none" />}
          onClick={onMinimize}
        />
        <IconButton
          aria-label="maximize"
          size="sm"
          variant="ghost"
          icon={<Icon as={isMaximized ? LuCopy : LuSquare} pointerEvents="none" />}
          onClick={onToggleMaximize}
        />
        <IconButton
          aria-label="close"
          size="sm"
          variant="ghost"
          colorScheme="red"
          icon={<Icon as={LuX} pointerEvents="none" />}
          onClick={onClose}
        />
      </HStack>
    </HStack>
  );
};

export default WindowTitleBar;
