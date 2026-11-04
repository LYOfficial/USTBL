import { Grid, GridItem, HStack, Icon, Text, VStack } from "@chakra-ui/react";
import { useRouter } from "next/router";
import React from "react";
import { useTranslation } from "react-i18next";
import { LuCirclePlus, LuRefreshCw, LuSettings } from "react-icons/lu";
import SelectableButton from "@/components/common/selectable-button";
import InstanceSidebar from "@/components/instance-sidebar";
import { useLauncherConfig } from "@/contexts/config";

interface InstancesLayoutProps {
  children: React.ReactNode;
}

const InstancesLayout: React.FC<InstancesLayoutProps> = ({ children }) => {
  const router = useRouter();
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const showNavBar = config.general.functionality.instancesNavType !== "hidden";

  return (
    <Grid templateColumns={showNavBar ? "1fr 3fr" : "3fr"} gap={4} h="100%">
      {showNavBar && (
        <GridItem className="content-full-y">
          <VStack align="stretch" h="100%" spacing={4}>
            <InstanceSidebar />
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
