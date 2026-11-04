import { Box, Collapse, HStack, Icon, Stack, Text } from "@chakra-ui/react";
import { useRouter } from "next/router";
import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { IconType } from "react-icons";
import {
  LuBookmark,
  LuBoxes,
  LuDownload,
  LuFolder,
  LuPackageCheck,
  LuStar,
} from "react-icons/lu";
import SelectableButton from "@/components/common/selectable-button";
import {
  InstanceRow,
  SectionHeader,
} from "@/components/instance-sidebar-parts";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { InstanceSummary } from "@/models/instance/misc";
import { InstanceService } from "@/services/instance";
import { getGameDirName } from "@/utils/instance";
import {
  Section,
  buildSidebar,
  toggleCollapsed,
} from "@/utils/instance-groups";

const isInstanceDetailsPage = (path: string) =>
  path.startsWith("/instances/details/");

const InstanceSidebar: React.FC = () => {
  const router = useRouter();
  const { t } = useTranslation();
  const toast = useToast();
  const { config, update } = useLauncherConfig();
  const { getInstanceList } = useGlobalData();
  const instanceList = getInstanceList();
  const groups = config.states.instanceGroups;
  const navType = config.general.functionality.instancesNavType;

  const [editing, setEditing] = useState<{
    sectionKey: string;
    instanceId: string;
  } | null>(null);

  const sections = useMemo(
    () =>
      buildSidebar(
        instanceList || [],
        groups,
        navType,
        config.localGameDirectories.map((dir) => ({
          name: dir.name,
          title: getGameDirName(dir),
        }))
      ),
    [instanceList, groups, navType, config.localGameDirectories]
  );

  // Truncate to the ID, excluding subpage routes
  const selectedKey = useMemo(() => {
    const parts = router.asPath.split("/");
    if (parts[2] === "details" && parts[3]) {
      return `/instances/details/${parts[3]}`;
    }
    return router.asPath;
  }, [router.asPath]);

  const sectionTitle = (section: Section) => {
    switch (section.key) {
      case "installing":
        return t("AllInstancesPage.installing");
      case "installed":
        return t("InstanceGroups.installed");
      case "starred":
        return t("InstanceGroups.starred");
      default:
        return section.title;
    }
  };

  const sectionIcon = (section: Section): IconType => {
    if (section.kind === "installing") return LuDownload;
    if (section.kind === "starred") return LuStar;
    if (section.kind === "custom") return LuBookmark;
    return section.key === "installed" ? LuPackageCheck : LuFolder;
  };

  const navigateToInstance = (instance: InstanceSummary) => {
    const value = `/instances/details/${encodeURIComponent(instance.id)}`;
    // keep the current subpage when switching instances on a details page
    if (isInstanceDetailsPage(router.asPath)) {
      router.push(`${value}/${router.pathname.split("/")[4] ?? ""}`);
    } else router.push(value);
  };

  const renameInstance = (instance: InstanceSummary, name: string) => {
    setEditing(null);
    const trimmed = name.trim();
    if (!trimmed || trimmed === instance.name) return;
    InstanceService.renameInstance(instance.id, trimmed).then((response) => {
      if (response.status === "success") getInstanceList(true);
      else
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
    });
  };

  const collapsed = groups.collapsed ?? [];

  return (
    <Stack flex="1" minH={0} align="stretch" spacing={2}>
      <SelectableButton
        size="sm"
        onClick={() => router.push("/instances/list")}
        isSelected={router.asPath.startsWith("/instances/list")}
      >
        <HStack spacing={2} overflow="hidden">
          <Icon as={LuBoxes} />
          <Text fontSize="sm" className="ellipsis-text">
            {t("AllInstancesPage.title")}
          </Text>
        </HStack>
      </SelectableButton>
      <Box flex="1" minH={0} overflowY="auto">
        {sections.map((section) => {
          const open = !collapsed.includes(section.key);
          return (
            <Box key={section.key} data-section-key={section.key}>
              <SectionHeader
                title={sectionTitle(section)}
                count={section.instances.length}
                icon={sectionIcon(section)}
                open={open}
                onToggle={() =>
                  update(
                    "states.instanceGroups",
                    toggleCollapsed(groups, section.key)
                  )
                }
              />
              <Collapse in={open} animateOpacity>
                <Stack align="stretch" spacing={0.5} pl={2}>
                  {section.instances.map((instance) => (
                    <InstanceRow
                      key={instance.id}
                      instance={instance}
                      selected={
                        selectedKey ===
                        `/instances/details/${encodeURIComponent(instance.id)}`
                      }
                      editing={
                        editing?.sectionKey === section.key &&
                        editing.instanceId === instance.id
                      }
                      onNavigate={() => navigateToInstance(instance)}
                      onStartEdit={() =>
                        setEditing({
                          sectionKey: section.key,
                          instanceId: instance.id,
                        })
                      }
                      onSubmitName={(name) => renameInstance(instance, name)}
                      onCancelEdit={() => setEditing(null)}
                    />
                  ))}
                  {section.instances.length === 0 &&
                    (section.kind === "starred" ||
                      section.kind === "custom") && (
                      <Text
                        className="secondary-text"
                        fontSize="xs"
                        px={3}
                        py={1}
                      >
                        {t("InstanceGroups.emptyHint")}
                      </Text>
                    )}
                </Stack>
              </Collapse>
            </Box>
          );
        })}
      </Box>
    </Stack>
  );
};

export default InstanceSidebar;
