import {
  Avatar,
  Badge,
  Box,
  Button,
  Flex,
  Spinner,
  Text,
} from "@chakra-ui/react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getAllWindows, getCurrentWindow } from "@tauri-apps/api/window";
import { exit } from "@tauri-apps/plugin-process";
import { useCallback, useEffect, useState } from "react";
import { VustbFriend } from "@/models/vustb";
import { AccountService } from "@/services/account";
import { ResourceAccelerationService } from "@/services/resource-acceleration";

type View = "menu" | "friends" | "notification";

const PANEL_BACKGROUND = "#3b4452";
const PANEL_TEXT = "#f3f4f6";
const PANEL_MUTED_TEXT = "#c8d0da";
const PANEL_SEPARATOR = "rgba(255, 255, 255, 0.2)";
const HOVER_BACKGROUND = "#536171";
const ACTIVE_BACKGROUND = "#2f78b7";

const closePopup = async () => {
  await getCurrentWindow().hide();
};

interface MenuItemProps {
  children: React.ReactNode;
  onClick: () => void;
  isLoading?: boolean;
  isDanger?: boolean;
}

const MenuItem = ({
  children,
  onClick,
  isLoading = false,
  isDanger = false,
}: MenuItemProps) => (
  <Button
    variant="unstyled"
    display="flex"
    alignItems="center"
    justifyContent="flex-start"
    w="100%"
    h="39px"
    px="16px"
    borderRadius="0"
    color={isDanger ? "#ffb4ae" : PANEL_TEXT}
    fontSize="14px"
    fontWeight="400"
    textAlign="left"
    whiteSpace="nowrap"
    isLoading={isLoading}
    _hover={{ bg: HOVER_BACKGROUND }}
    _active={{ bg: ACTIVE_BACKGROUND }}
    _focusVisible={{ boxShadow: `inset 0 0 0 1px ${ACTIVE_BACKGROUND}` }}
    onClick={onClick}
  >
    <Text noOfLines={1}>{children}</Text>
  </Button>
);

const ViewTransition = ({
  children,
  view,
}: {
  children: React.ReactNode;
  view: View;
}) => (
  <Box
    key={view}
    h="100%"
    animation="tray-popup-view-enter 180ms ease-out"
    sx={{
      "@keyframes tray-popup-view-enter": {
        from: {
          opacity: 0,
          transform: `translateX(${view === "friends" ? "20px" : "-20px"})`,
        },
        to: {
          opacity: 1,
          transform: "translateX(0)",
        },
      },
    }}
  >
    {children}
  </Box>
);

export default function TrayPopup() {
  const [view, setView] = useState<View>("menu");
  const [message, setMessage] = useState("");
  const [friends, setFriends] = useState<VustbFriend[]>([]);
  const [loading, setLoading] = useState(false);
  const [accelerationRunning, setAccelerationRunning] = useState(false);
  const [messageQuiet, setMessageQuiet] = useState(false);

  const resizePopup = useCallback(async (nextView: View) => {
    await invoke("resize_tray_popup", { view: nextView });
  }, []);

  const changeView = useCallback(
    (nextView: View) => {
      setView(nextView);
      void resizePopup(nextView);
    },
    [resizePopup]
  );

  const loadFriends = useCallback(async () => {
    setLoading(true);
    const response = await AccountService.retrieveVustbFriends();
    if (response.status === "success") {
      setFriends(
        [...response.data].sort((a, b) => Number(b.online) - Number(a.online))
      );
    }
    setLoading(false);
  }, []);

  useEffect(() => {
    let disposed = false;
    const setup = async () => {
      const unlisten = await listen<{ view: View; message?: string }>(
        "tray-popup-open",
        (event) => {
          if (disposed) return;
          setView(event.payload.view);
          setMessage(event.payload.message || "");
          void resizePopup(event.payload.view);
          if (event.payload.view === "notification") {
            window.setTimeout(() => void closePopup(), 4500);
          }
        }
      );
      return unlisten;
    };
    const cleanup = setup();
    return () => {
      disposed = true;
      void cleanup.then((unlisten) => unlisten());
    };
  }, [resizePopup]);

  useEffect(() => {
    if (view === "friends") void loadFriends();
  }, [loadFriends, view]);

  useEffect(() => {
    if (view !== "menu") return;
    void ResourceAccelerationService.status().then((response) => {
      if (response.status === "success")
        setAccelerationRunning(response.data.running);
    });
  }, [view]);

  useEffect(() => {
    setMessageQuiet(window.localStorage.getItem("ustbl.message.quiet.global") === "true");
  }, [view]);

  const startAcceleration = async () => {
    setLoading(true);
    const response = await ResourceAccelerationService.start(true);
    setLoading(false);
    if (response.status === "success") {
      setAccelerationRunning(true);
      setMessage("GitHub 加速已启动");
    } else {
      setMessage(response.details || "GitHub 加速启动失败");
    }
    changeView("notification");
  };

  const stopAcceleration = async () => {
    setLoading(true);
    const response = await ResourceAccelerationService.stop();
    setLoading(false);
    if (response.status === "success") setAccelerationRunning(false);
  };

  const triggerMain = async (event: string) => {
    if (
      event === "ustbl:tray-show-main" ||
      event === "ustbl:tray-launch-request"
    ) {
      const main = (await getAllWindows()).find(
        (window) => window.label === "main"
      );
      await main?.show();
      await main?.setFocus();
    }
    await emit(event);
    await closePopup();
  };

  const renderMenu = () => (
    <Box>
      <MenuItem onClick={() => void triggerMain("ustbl:tray-show-main")}>
        打开 USTBL
      </MenuItem>
      <MenuItem onClick={() => void triggerMain("ustbl:tray-launch-request")}>
        启动游戏
      </MenuItem>
      <Box borderTop="1px solid" borderColor={PANEL_SEPARATOR} />
      <MenuItem onClick={() => changeView("friends")}>好友列表</MenuItem>
      <Box borderTop="1px solid" borderColor={PANEL_SEPARATOR} />
      <MenuItem onClick={() => { const next = !messageQuiet; window.localStorage.setItem("ustbl.message.quiet.global", String(next)); setMessageQuiet(next); }}>{messageQuiet ? "开启消息提醒" : "消息免打扰"}</MenuItem>
      <Box borderTop="1px solid" borderColor={PANEL_SEPARATOR} />
      <MenuItem
        isLoading={loading}
        onClick={() =>
          void (accelerationRunning ? stopAcceleration() : startAcceleration())
        }
      >
        {accelerationRunning ? "关闭资源加速" : "启用资源加速"}
      </MenuItem>
      <Box borderTop="1px solid" borderColor={PANEL_SEPARATOR} />
      <MenuItem isDanger onClick={() => void exit(0)}>
        退出 USTBL
      </MenuItem>
    </Box>
  );

  const renderNotification = () => (
    <Flex direction="column" h="100%">
      <Box flex="1" px={4} py={5}>
        <Text fontSize="16px" fontWeight="600" color={PANEL_TEXT} mb={2}>
          {message.includes("失败") ? "加速启动失败" : "消息提醒"}
        </Text>
        <Text color={PANEL_MUTED_TEXT} fontSize="14px" lineHeight="1.5">
          {message}
        </Text>
      </Box>
      <Box borderTop="1px solid" borderColor={PANEL_SEPARATOR}>
        <MenuItem onClick={() => changeView("menu")}>返回</MenuItem>
      </Box>
    </Flex>
  );

  const renderFriends = () => (
    <Flex direction="column" h="100%" minH={0}>
      <Box px={4} py={3} borderBottom="1px solid" borderColor={PANEL_SEPARATOR}>
        <Text color={PANEL_TEXT} fontSize="16px" fontWeight="600">
          好友列表
        </Text>
      </Box>
      <Box
        flex="1"
        minH={0}
        overflowY="auto"
        px={2}
        py={2}
        css={{
          "&::-webkit-scrollbar": { width: "8px" },
          "&::-webkit-scrollbar-thumb": {
            background: "#687586",
            borderRadius: "4px",
          },
          "&::-webkit-scrollbar-track": { background: "transparent" },
        }}
      >
        {loading && friends.length === 0 ? (
          <Flex justify="center" py={6}>
            <Spinner color="#9bb8d0" />
          </Flex>
        ) : friends.length === 0 ? (
          <Text
            color={PANEL_MUTED_TEXT}
            fontSize="14px"
            textAlign="center"
            py={6}
          >
            暂无好友
          </Text>
        ) : (
          friends.map((friend) => (
            <Flex
              key={friend.id}
              align="center"
              gap={3}
              minH="58px"
              px={2}
              py={2}
              borderRadius="2px"
              _hover={{ bg: HOVER_BACKGROUND }}
            >
              <Avatar
                size="sm"
                src={friend.avatarUrl}
                name={friend.displayName}
              />
              <Box flex="1" minW={0}>
                <Text color={PANEL_TEXT} fontSize="14px" noOfLines={1}>
                  {friend.displayName}
                </Text>
                {friend.online && friend.instanceName && (
                  <Text color="#9dd49f" fontSize="12px" noOfLines={1}>
                    正在玩 {friend.instanceName}
                  </Text>
                )}
              </Box>
              <Badge
                colorScheme={friend.online ? "green" : "gray"}
                flexShrink={0}
              >
                {friend.online ? "在线" : "离线"}
              </Badge>
            </Flex>
          ))
        )}
      </Box>
      <Flex
        gap={2}
        px={2}
        py={2}
        borderTop="1px solid"
        borderColor={PANEL_SEPARATOR}
      >
        <Button
          flex="1"
          h="36px"
          variant="unstyled"
          color={PANEL_TEXT}
          fontSize="14px"
          borderRadius="2px"
          isLoading={loading}
          _hover={{ bg: HOVER_BACKGROUND }}
          _active={{ bg: ACTIVE_BACKGROUND }}
          onClick={() => void loadFriends()}
        >
          刷新
        </Button>
        <Button
          flex="1"
          h="36px"
          variant="unstyled"
          color={PANEL_TEXT}
          fontSize="14px"
          borderRadius="2px"
          _hover={{ bg: HOVER_BACKGROUND }}
          _active={{ bg: ACTIVE_BACKGROUND }}
          onClick={() => changeView("menu")}
        >
          返回
        </Button>
      </Flex>
    </Flex>
  );

  return (
    <Box
      h="100vh"
      overflow="hidden"
      bg={PANEL_BACKGROUND}
      color={PANEL_TEXT}
      border="1px solid"
      borderColor="rgba(255, 255, 255, 0.16)"
      borderRadius="4px"
      boxShadow="0 6px 18px rgba(0, 0, 0, 0.38)"
    >
      <ViewTransition view={view}>
        {view === "menu" && renderMenu()}
        {view === "friends" && renderFriends()}
        {view === "notification" && renderNotification()}
      </ViewTransition>
    </Box>
  );
}
