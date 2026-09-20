import {
  Avatar,
  Box,
  Button,
  HStack,
  Icon,
  Progress,
  Text,
  Tooltip,
  VStack,
} from "@chakra-ui/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useCallback, useEffect, useRef, useState } from "react";
import { LuCalendarCheck, LuRefreshCw } from "react-icons/lu";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { VustbAccount } from "@/models/vustb";
import { AccountService } from "@/services/account";

const groupLabels: Record<string, string> = {
  super_admin: "超级管理员",
  admin: "管理员",
  platform_manager: "平台管理员",
  server_manager: "服务器管理员",
  content_manager: "内容管理员",
  teacher: "老师",
  user: "用户",
};

const userGroupLabel = (group: string) => groupLabels[group] || group || "用户";

const VustbAccountPanel = ({
  refreshKey = 0,
  onAccountChange,
}: {
  refreshKey?: number;
  onAccountChange?: (account: VustbAccount | null) => void;
}) => {
  const toast = useToast();
  const { getAuthServerList, getPlayerList } = useGlobalData();
  const [account, setAccount] = useState<VustbAccount | null>(null);
  const [isLoggingIn, setIsLoggingIn] = useState(false);
  const [isSyncing, setIsSyncing] = useState(false);
  const [isCheckingIn, setIsCheckingIn] = useState(false);
  const [isLoggingOut, setIsLoggingOut] = useState(false);
  const accountRequest = useRef(0);

  const loadAccount = useCallback(async () => {
    const request = ++accountRequest.current;
    const response = await AccountService.retrieveVustbAccount();
    if (request !== accountRequest.current || response.status !== "success")
      return;
    setAccount(response.data);
    if (response.data) {
      const refreshed = await AccountService.refreshVustbAccount();
      if (request === accountRequest.current && refreshed.status === "success")
        setAccount(refreshed.data);
    }
  }, []);

  useEffect(() => {
    const requests = accountRequest;
    loadAccount();
    return () => {
      requests.current++;
    };
  }, [loadAccount, refreshKey]);

  useEffect(() => {
    onAccountChange?.(account);
  }, [account, onAccountChange]);

  const handleLogin = async () => {
    accountRequest.current++;
    setIsLoggingIn(true);
    const codeResponse = await AccountService.fetchVustbOAuthCode();
    if (codeResponse.status !== "success") {
      toast({
        title: codeResponse.message,
        description: codeResponse.details,
        status: "error",
      });
      setIsLoggingIn(false);
      return;
    }

    try {
      await openUrl(codeResponse.data.verificationUri);
      const loginResponse = await AccountService.loginVustbAccount(
        codeResponse.data
      );
      if (loginResponse.status === "success") {
        setAccount(loginResponse.data);
        getPlayerList(true);
        getAuthServerList(true);
        toast({ title: "像素北科账号登录成功", status: "success" });
      } else {
        toast({
          title: loginResponse.message,
          description: loginResponse.details,
          status: "error",
        });
      }
    } catch (error) {
      toast({
        title: "无法打开像素北科登录页面",
        description: String(error),
        status: "error",
      });
    } finally {
      setIsLoggingIn(false);
    }
  };

  const handleSync = async () => {
    accountRequest.current++;
    setIsSyncing(true);
    const response = await AccountService.syncVustbAccount();
    if (response.status === "success") {
      setAccount(response.data);
      getPlayerList(true);
      toast({ title: "像素北科账户资料已同步", status: "success" });
    } else if (String(response.raw_error) === "EXPIRED") {
      toast({
        title: "像素北科登录已过期",
        description: "请在浏览器中重新授权，完成后会自动恢复账户同步。",
        status: "warning",
      });
      setIsSyncing(false);
      await handleLogin();
      return;
    } else {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
    }
    setIsSyncing(false);
  };

  const handleLogout = async () => {
    accountRequest.current++;
    setIsLoggingOut(true);
    const response = await AccountService.logoutVustbAccount();
    if (response.status === "success") {
      setAccount(null);
      getPlayerList(true);
      getAuthServerList(true);
      toast({ title: "已注销像素北科账号", status: "success" });
    } else {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
    }
    setIsLoggingOut(false);
  };

  const handleCheckin = async () => {
    accountRequest.current++;
    setIsCheckingIn(true);
    const response = await AccountService.checkinVustbAccount();
    if (response.status === "success") {
      setAccount(response.data.account);
      toast({ title: response.data.message || "签到成功", status: "success" });
    } else {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
    }
    setIsCheckingIn(false);
  };

  if (!account) {
    return (
      <Box
        minH="80px"
        display="flex"
        alignItems="center"
        justifyContent="center"
      >
        <Button
          colorScheme="blue"
          onClick={handleLogin}
          isLoading={isLoggingIn}
          loadingText="请在浏览器完成登录"
        >
          登录像素北科账号
        </Button>
      </Box>
    );
  }

  const dateKey = (value: Date) =>
    new Intl.DateTimeFormat("en-CA", {
      timeZone: "Asia/Shanghai",
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    }).format(value);
  const checkedInToday = account.lastCheckin
    ? dateKey(new Date(account.lastCheckin)) === dateKey(new Date())
    : false;
  const progress = account.progression.isMaxLevel
    ? 100
    : account.progression.nextLevelExperience > 0
      ? (account.progression.experience /
          account.progression.nextLevelExperience) *
        100
      : 0;
  const playHours = Math.floor(account.progression.playTimeSeconds / 3600);
  const playMinutes = Math.floor(
    (account.progression.playTimeSeconds % 3600) / 60
  );

  return (
    <Box minH="112px" px={{ base: 6, md: 8 }} py={3}>
      <HStack spacing={3} justify="space-between">
        <HStack minW={0} spacing={3}>
          <Avatar
            src={account.avatarUrl}
            name={account.username}
            boxSize="58px"
            borderRadius="sm"
            borderWidth="2px"
            borderColor="whiteAlpha.700"
            boxShadow="0 6px 18px rgba(0, 0, 0, 0.30)"
            sx={{ "& > img": { borderRadius: "inherit" } }}
          />
          <Box minW={0}>
            <Text fontWeight="semibold" className="ellipsis-text">
              {account.username}
            </Text>
            <Text fontSize="sm" className="secondary-text">
              {userGroupLabel(account.userGroup)} · {account.profiles.length}{" "}
              个游戏角色
            </Text>
            <Text fontSize="sm" className="secondary-text">
              像素积分 {account.pixelPoints ?? 0} · 贝壳积分{" "}
              {account.shellPoints ?? 0}
            </Text>
          </Box>
        </HStack>
        <HStack flexShrink={0}>
          <Tooltip label={checkedInToday ? "今天已签到" : "每日签到"}>
            <Button
              variant="ghost"
              size="sm"
              onClick={handleCheckin}
              isLoading={isCheckingIn}
              isDisabled={checkedInToday}
              aria-label="像素北科每日签到"
            >
              <Icon as={LuCalendarCheck} />
            </Button>
          </Tooltip>
          <Tooltip label="同步账户资料与游戏角色">
            <Button
              variant="ghost"
              size="sm"
              onClick={handleSync}
              isLoading={isSyncing}
              aria-label="同步像素北科账户"
            >
              <Icon as={LuRefreshCw} />
            </Button>
          </Tooltip>
          <Button
            colorScheme="gray"
            variant="solid"
            size="sm"
            onClick={handleLogout}
            isLoading={isLoggingOut}
          >
            注销登录
          </Button>
        </HStack>
      </HStack>
      <VStack align="stretch" spacing={1} mt={2}>
        <HStack justify="space-between" fontSize="xs">
          <Text fontWeight="semibold" color="green.300">
            Lv. {account.progression.level}
          </Text>
          <Text className="secondary-text">
            {account.progression.isMaxLevel
              ? `${account.progression.experience} XP · 满级`
              : `${account.progression.experience} / ${account.progression.nextLevelExperience} XP`}
          </Text>
        </HStack>
        <Progress
          value={progress}
          size="xs"
          colorScheme="green"
          borderRadius="full"
          aria-label={`等级 ${account.progression.level} 经验进度`}
        />
        <Text fontSize="xs" className="secondary-text">
          累计签到 {account.progression.checkinDays} 天 · 游玩 {playHours} 小时{" "}
          {playMinutes} 分钟
        </Text>
      </VStack>
    </Box>
  );
};

export default VustbAccountPanel;
