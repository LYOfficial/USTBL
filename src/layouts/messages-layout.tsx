import { Grid, GridItem, HStack, Icon, Text, VStack } from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useTranslation } from "react-i18next";
import { LuBell, LuMessageCircle, LuServer } from "react-icons/lu";
import NavMenu from "@/components/common/nav-menu";

const MessagesLayout: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const router = useRouter();
  const { t } = useTranslation();
  const items = [
    { route: "/messages/announcements", key: "announcement", icon: LuBell },
    { route: "/messages/server", key: "server", icon: LuServer },
    { route: "/messages/friends", key: "friends", icon: LuMessageCircle },
  ];
  return (
    <Grid templateColumns="1fr 3fr" gap={4} h="100%">
      <GridItem className="content-full-y">
        <VStack align="stretch" spacing={4}>
          <NavMenu
            selectedKeys={[router.asPath]}
            onClick={(value) => void router.push(value)}
            items={items.map((item) => ({
              value: item.route,
              label: <HStack spacing={2}><Icon as={item.icon} /><Text fontSize="sm">{t(`MessagesPage.${item.key}`)}</Text></HStack>,
            }))}
          />
        </VStack>
      </GridItem>
      <GridItem className="content-full-y">
        <VStack align="stretch" spacing={4} h="100%">{children}</VStack>
      </GridItem>
    </Grid>
  );
};

export default MessagesLayout;
