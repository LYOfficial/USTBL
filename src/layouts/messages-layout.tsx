import { Box, Button, Center, HStack, Icon, Text, VStack } from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useTranslation } from "react-i18next";
import { LuBell, LuMessageCircle, LuServer } from "react-icons/lu";
import { useEffect, useState } from "react";
import { AccountService } from "@/services/account";

const MessagesLayout: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const router = useRouter();
  const { t } = useTranslation();
  const [accountState, setAccountState] = useState<boolean | null>(null);
  const [accountError, setAccountError] = useState<string>();
  const items = [
    { route: "/messages/announcements", key: "announcement", icon: LuBell },
    { route: "/messages/server", key: "server", icon: LuServer },
    { route: "/messages/friends", key: "friends", icon: LuMessageCircle },
  ];
  useEffect(() => {
    let disposed = false;
    void AccountService.retrieveVustbAccount().then((response) => {
      if (disposed) return;
      if (response.status === "success") {
        setAccountState(Boolean(response.data));
        setAccountError(undefined);
      } else {
        setAccountState(false);
        setAccountError(response.details || response.message || "账号状态获取失败，请稍后重试");
      }
    });
    return () => { disposed = true; };
  }, []);
  return (
    <VStack align="stretch" spacing={4} h="100%" minH={0}>
      <HStack w="100%" spacing={0} flexShrink={0} borderBottom="1px solid" borderColor="blackAlpha.200" pb={2}>
        {items.map((item) => (
          <Button
            key={item.route}
            variant={router.pathname === item.route ? "solid" : "ghost"}
            colorScheme={router.pathname === item.route ? "blue" : undefined}
            flex={1}
            justifyContent="center"
            borderRadius={0}
            onClick={() => void router.push(item.route)}
            leftIcon={<Icon as={item.icon} />}
          >
            {t(`MessagesPage.${item.key}`)}
          </Button>
        ))}
      </HStack>
      <Box flex={1} minH={0}>
        {accountState === null ? <Center h="100%"><Text>正在检查像素北科账号…</Text></Center> : accountError ? <Center h="100%"><VStack spacing={4}><Text color="red.500">{accountError}</Text><Button variant="outline" onClick={() => void router.reload()}>重新加载</Button></VStack></Center> : accountState ? children : <Center h="100%"><VStack spacing={4}><Text>请先登录像素北科账号后再使用消息功能</Text><Button colorScheme="blue" onClick={() => void router.push("/accounts")}>前往账户页面登录</Button></VStack></Center>}
      </Box>
    </VStack>
  );
};

export default MessagesLayout;
