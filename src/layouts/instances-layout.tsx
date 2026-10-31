import {
  Box,
  Button,
  Collapse,
  Grid,
  GridItem,
  HStack,
  Icon,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { IconType } from "react-icons";
import { FaStar } from "react-icons/fa6";
import {
  LuBox,
  LuBoxes,
  LuChevronDown,
  LuChevronRight,
  LuCirclePlus,
  LuDownload,
  LuFolder,
  LuRefreshCw,
  LuSettings,
} from "react-icons/lu";
import NavMenu from "@/components/common/nav-menu";
import SelectableButton from "@/components/common/selectable-button";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { InstanceSummary } from "@/models/instance/misc";
import { instanceNavigationGroups } from "@/utils/installing-instance";
import { getGameDirName } from "@/utils/instance";

interface InstancesLayoutProps {
  children: React.ReactNode;
}

const InstancesLayout: React.FC<InstancesLayoutProps> = ({ children }) => {
  const router = useRouter();
  const { t } = useTranslation();
  const { getInstanceList } = useGlobalData();
  const instanceList = getInstanceList() || [];
  const { config } = useLauncherConfig();
  const navBarType = config.general.functionality.instancesNavType;
  const showNavBar = navBarType !== "hidden";

  const groups = instanceNavigationGroups(instanceList);
  const [allOpen, setAllOpen] = useState(true);
  const [installingOpen, setInstallingOpen] = useState(true);
  const instanceItems = (instances: InstanceSummary[]) =>
    instances.map((item) => ({
      key: `details/${encodeURIComponent(item.id)}`,
      icon: item.starred ? FaStar : LuBox,
      label: item.name,
    }));

  // Truncate to the ID, excluding subpage routes
  const isInstanceDetailsPage = (path: string) =>
    path.startsWith("/instances/details/");

  const selectedKey = useMemo(() => {
    const parts = router.asPath.split("/");
    if (parts[2] === "details" && parts[3]) {
      return `/instances/details/${parts[3]}`;
    }
    return router.asPath;
  }, [router.asPath]);

  const renderMenu = (
    items: { key: string; icon: IconType; label: string }[]
  ) => (
    <NavMenu
      selectedKeys={[selectedKey]}
      onClick={(value) => {
        if (
          isInstanceDetailsPage(router.asPath) &&
          isInstanceDetailsPage(value)
        ) {
          router.push(`${value}/${router.pathname.split("/")[4] ?? ""}`);
        } else router.push(value);
      }}
      items={items.map((item) => ({
        label: (
          <HStack spacing={2} overflow="hidden">
            <Icon as={item.icon} />
            <Text fontSize="sm" className="ellipsis-text">
              {item.label}
            </Text>
          </HStack>
        ),
        value: `/instances/${item.key}`,
        tooltip: item.label,
      }))}
    />
  );

  const groupHeader = (
    label: string,
    count: number,
    open: boolean,
    toggle: () => void,
    icon: IconType
  ) => (
    <Button
      variant="ghost"
      size="sm"
      w="100%"
      justifyContent="start"
      onClick={toggle}
      aria-expanded={open}
      leftIcon={<Icon as={open ? LuChevronDown : LuChevronRight} />}
    >
      <HStack spacing={2}>
        <Icon as={icon} />
        <Text>{label}</Text>
        <Text className="secondary-text" fontSize="xs">
          {count}
        </Text>
      </HStack>
    </Button>
  );

  return (
    <Grid templateColumns={showNavBar ? "1fr 3fr" : "3fr"} gap={4} h="100%">
      {showNavBar && (
        <GridItem className="content-full-y">
          <VStack align="stretch" h="100%" spacing={4}>
            <Box flex="1" minH={0} overflowY="auto">
              {groupHeader(
                t("AllInstancesPage.title"),
                groups.allCount,
                allOpen,
                () => setAllOpen(!allOpen),
                LuBoxes
              )}
              <Collapse in={allOpen} animateOpacity>
                <Box pl={2}>
                  {renderMenu([
                    {
                      key: "list",
                      icon: LuBoxes,
                      label: t("AllInstancesPage.title"),
                    },
                    ...(navBarType === "instance"
                      ? instanceItems(groups.all)
                      : config.localGameDirectories.map((item) => ({
                          key: `list/${encodeURIComponent(item.name)}`,
                          icon: LuFolder,
                          label: getGameDirName(item),
                        }))),
                  ])}
                </Box>
              </Collapse>
              {groups.installingCount > 0 && (
                <>
                  {groupHeader(
                    t("AllInstancesPage.installing"),
                    groups.installingCount,
                    installingOpen,
                    () => setInstallingOpen(!installingOpen),
                    LuDownload
                  )}
                  <Collapse in={installingOpen} animateOpacity>
                    <Box pl={2}>
                      {renderMenu(instanceItems(groups.installing))}
                    </Box>
                  </Collapse>
                </>
              )}
            </Box>
            <VStack mt="auto" align="stretch" spacing={0.5}>
              <SelectableButton
                size="sm"
                onClick={() => {
                  router.push("/instances/sync");
                }}
                isSelected={router.asPath === "/instances/sync"}
              >
                <HStack spacing={2} overflow="hidden">
                  <Icon as={LuRefreshCw} />
                  <Text fontSize="sm" className="ellipsis-text">
                    {t("InstanceSyncPage.title")}
                  </Text>
                </HStack>
              </SelectableButton>
              <SelectableButton
                size="sm"
                onClick={() => {
                  router.push("/instances/add-import");
                }}
                isSelected={router.asPath === "/instances/add-import"}
              >
                <HStack spacing={2} overflow="hidden">
                  <Icon as={LuCirclePlus} />
                  <Text fontSize="sm" className="ellipsis-text">
                    {t("AllInstancesPage.button.addAndImport")}
                  </Text>
                </HStack>
              </SelectableButton>
              <SelectableButton
                size="sm"
                onClick={() => {
                  router.push("/settings/global-game");
                }}
              >
                <HStack spacing={2} overflow="hidden">
                  <Icon as={LuSettings} />
                  <Text fontSize="sm" className="ellipsis-text">
                    {t("SettingsLayout.settingsDomainList.global-game")}
                  </Text>
                </HStack>
              </SelectableButton>
            </VStack>
          </VStack>
        </GridItem>
      )}
      <GridItem className="content-full-y">{children}</GridItem>
    </Grid>
  );
};

export default InstancesLayout;
