import {
  Avatar,
  Badge,
  Box,
  Button,
  CloseButton,
  Flex,
  Heading,
  HStack,
  Icon,
  Spinner,
  Text,
  VStack,
  useColorModeValue,
} from "@chakra-ui/react";
import {
  getCurrentWindow,
  LogicalSize,
  PhysicalPosition,
  currentMonitor,
} from "@tauri-apps/api/window";
import { getAllWindows } from "@tauri-apps/api/window";
import { emit, listen } from "@tauri-apps/api/event";
import { exit } from "@tauri-apps/plugin-process";
import { useCallback, useEffect, useState } from "react";
import {
  LuGamepad2,
  LuGauge,
  LuLogOut,
  LuPlay,
  LuRefreshCw,
  LuSquareArrowOutUpRight,
  LuUsersRound,
} from "react-icons/lu";
import { AccountService } from "@/services/account";
import { ResourceAccelerationService } from "@/services/resource-acceleration";
import { VustbFriend } from "@/models/vustb";

type View = "menu" | "friends" | "notification";

const closePopup = async () => {
  await getCurrentWindow().hide();
};

export default function TrayPopup() {
  const [view, setView] = useState<View>("menu");
  const [message, setMessage] = useState("");
  const [friends, setFriends] = useState<VustbFriend[]>([]);
  const [loading, setLoading] = useState(false);
  const [accelerationRunning, setAccelerationRunning] = useState(false);
  const panelBackground = useColorModeValue("white", "gray.900");
  const panelText = useColorModeValue("gray.800", "white");
  const panelBorder = useColorModeValue("gray.200", "whiteAlpha.200");
  const itemBackground = useColorModeValue("gray.50", "whiteAlpha.100");

  const resizePopup = useCallback(async (nextView: View) => {
    const width = nextView === "friends" ? 420 : 260;
    const height = nextView === "friends" ? 460 : nextView === "notification" ? 220 : 280;
    const currentWindow = getCurrentWindow();
    await currentWindow.setSize(new LogicalSize(width, height));
    const monitor = await currentMonitor();
    if (monitor) {
      const physicalWidth = Math.ceil(width * monitor.scaleFactor);
      const physicalHeight = Math.ceil(height * monitor.scaleFactor);
      const margin = Math.ceil(12 * monitor.scaleFactor);
      await currentWindow.setPosition(
        new PhysicalPosition(
          monitor.workArea.position.x + monitor.workArea.size.width - physicalWidth - margin,
          monitor.workArea.position.y + monitor.workArea.size.height - physicalHeight - margin
        )
      );
    }
  }, []);

  const loadFriends = useCallback(async () => {
    setLoading(true);
    const response = await AccountService.retrieveVustbFriends();
    if (response.status === "success") {
      setFriends([...response.data].sort((a, b) => Number(b.online) - Number(a.online)));
    }
    setLoading(false);
  }, []);

  useEffect(() => {
    let disposed = false;
    const setup = async () => {
      const unlisten = await listen<{ view: View; message?: string }>("tray-popup-open", (event) => {
        if (disposed) return;
        setView(event.payload.view);
        setMessage(event.payload.message || "");
        void resizePopup(event.payload.view);
        if (event.payload.view === "notification") {
          window.setTimeout(() => void closePopup(), 4500);
        }
      });
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
      if (response.status === "success") setAccelerationRunning(response.data.running);
    });
  }, [view]);

  const startAcceleration = async () => {
    setLoading(true);
    const response = await ResourceAccelerationService.start(true);
    setLoading(false);
    if (response.status === "success") {
      setAccelerationRunning(true);
      setView("notification");
      void resizePopup("notification");
      setMessage("GitHub 加速已启动");
    } else {
      setView("notification");
      void resizePopup("notification");
      setMessage(response.details || "GitHub 加速启动失败");
    }
  };

  const stopAcceleration = async () => {
    setLoading(true);
    const response = await ResourceAccelerationService.stop();
    setLoading(false);
    if (response.status === "success") setAccelerationRunning(false);
  };

  const triggerMain = async (event: string) => {
    if (event === "ustbl:tray-show-main" || event === "ustbl:tray-launch-request") {
      const main = (await getAllWindows()).find((window) => window.label === "main");
      await main?.show();
      await main?.setFocus();
    }
    await emit(event);
    await closePopup();
  };

  return (
    <Box bg={panelBackground} color={panelText} borderWidth="1px" borderColor={panelBorder} borderRadius="lg" boxShadow="dark-lg" p={3} h="100vh" overflow="hidden">
      <Flex align="center" justify="space-between" mb={3}>
        <HStack spacing={2}><Icon as={view === "friends" ? LuUsersRound : view === "notification" ? LuGauge : LuGamepad2} color="blue.300" /><Heading size="sm">USTBL</Heading></HStack>
        <CloseButton onClick={() => void closePopup()} />
      </Flex>
      {view === "menu" && <VStack align="stretch" spacing={1}>
        <Button size="sm" variant="outline" colorScheme="blue" leftIcon={<LuSquareArrowOutUpRight />} justifyContent="flex-start" onClick={() => void triggerMain("ustbl:tray-show-main")}>打开 USTBL</Button>
        <Button size="sm" variant="outline" colorScheme="blue" leftIcon={<LuPlay />} justifyContent="flex-start" onClick={() => void triggerMain("ustbl:tray-launch-request")}>启动游戏</Button>
        <Button size="sm" variant="outline" colorScheme="blue" leftIcon={<LuUsersRound />} justifyContent="flex-start" onClick={() => { setView("friends"); void resizePopup("friends"); }}>查看好友列表</Button>
        <Button size="sm" variant="outline" colorScheme="blue" justifyContent="flex-start" leftIcon={<LuGauge />} isLoading={loading} onClick={() => void (accelerationRunning ? stopAcceleration() : startAcceleration())}>{accelerationRunning ? "关闭资源加速" : "启用资源加速"}</Button>
        <Button size="sm" variant="outline" colorScheme="red" leftIcon={<LuLogOut />} justifyContent="flex-start" onClick={() => void exit(0)}>退出</Button>
      </VStack>}
      {view === "notification" && <VStack align="stretch" spacing={3} py={5}><Heading size="md">{message.includes("失败") ? "加速启动失败" : "GitHub 加速"}</Heading><Text opacity={0.75}>{message}</Text><Button size="sm" variant="outline" colorScheme="blue" onClick={() => { setView("menu"); void resizePopup("menu"); }}>返回</Button></VStack>}
      {view === "friends" && <VStack align="stretch" spacing={2} overflowY="auto" maxH="380px">
        <Button size="sm" leftIcon={<LuRefreshCw />} isLoading={loading} onClick={() => void loadFriends()}>刷新</Button>
        {loading && friends.length === 0 ? <Spinner alignSelf="center" /> : friends.map((friend) => <HStack key={friend.id} p={2} bg={itemBackground} borderRadius="md"><Avatar size="sm" src={friend.avatarUrl} name={friend.displayName} /><Box flex={1}><Text fontSize="sm">{friend.displayName}</Text>{friend.online && friend.instanceName && <Text fontSize="xs" color="green.500">正在玩 {friend.instanceName}</Text>}</Box><Badge colorScheme={friend.online ? "green" : "gray"}>{friend.online ? "在线" : "离线"}</Badge></HStack>)}
        <Button size="sm" variant="outline" colorScheme="blue" onClick={() => { setView("menu"); void resizePopup("menu"); }}>返回</Button>
      </VStack>}
    </Box>
  );
}
