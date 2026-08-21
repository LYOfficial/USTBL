import {
  Avatar,
  Box,
  Button,
  HStack,
  Icon,
  Text,
  Tooltip,
} from "@chakra-ui/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useCallback, useEffect, useState } from "react";
import { LuRefreshCw } from "react-icons/lu";
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

const VustbAccountPanel = () => {
  const toast = useToast();
  const { getAuthServerList, getPlayerList } = useGlobalData();
  const [account, setAccount] = useState<VustbAccount | null>(null);
  const [isLoggingIn, setIsLoggingIn] = useState(false);
  const [isSyncing, setIsSyncing] = useState(false);
  const [isLoggingOut, setIsLoggingOut] = useState(false);

  const loadAccount = useCallback(async () => {
    const response = await AccountService.retrieveVustbAccount();
    if (response.status === "success") setAccount(response.data);
  }, []);

  useEffect(() => {
    loadAccount();
  }, [loadAccount]);

  const handleLogin = async () => {
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
    setIsSyncing(true);
    const response = await AccountService.syncVustbAccount();
    if (response.status === "success") {
      setAccount(response.data);
      toast({ title: "像素北科账户资料已同步", status: "success" });
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

  return (
    <HStack
      minH="80px"
      pr={2}
      pl={{ base: 6, md: 8 }}
      spacing={3}
      justify="space-between"
    >
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
        </Box>
      </HStack>
      <HStack flexShrink={0}>
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
  );
};

export default VustbAccountPanel;
