import {
  Box,
  Collapse,
  HStack,
  Icon,
  Menu,
  MenuButton,
  MenuItem,
  MenuList,
  Stack,
  Text,
  useColorModeValue,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import React, { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { IconType } from "react-icons";
import {
  LuBookmark,
  LuBoxes,
  LuDownload,
  LuFolder,
  LuPackageCheck,
  LuPencil,
  LuPin,
  LuPinOff,
  LuPlus,
  LuStar,
  LuTrash2,
} from "react-icons/lu";
import SelectableButton from "@/components/common/selectable-button";
import {
  InstanceRow,
  SectionHeader,
} from "@/components/instance-sidebar-parts";
import GenericConfirmDialog from "@/components/modals/generic-confirm-dialog";
import NewInstanceGroupModal from "@/components/modals/new-instance-group-modal";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import {
  InstanceDrop,
  SectionMove,
  useSidebarDrag,
} from "@/hooks/sidebar-drag";
import { InstanceSummary } from "@/models/instance/misc";
import { InstanceService } from "@/services/instance";
import { getGameDirName } from "@/utils/instance";
import {
  DropResult,
  Section,
  addToNewGroup,
  buildSidebar,
  deleteGroup,
  dropInstance,
  isGroupNameTaken,
  isSortableSection,
  moveSection,
  newGroupId,
  normalizeGroups,
  renameGroup,
  sameGroups,
  toggleCollapsed,
  togglePinned,
} from "@/utils/instance-groups";

const isInstanceDetailsPage = (path: string) =>
  path.startsWith("/instances/details/");

const InstanceSidebar: React.FC = () => {
  const router = useRouter();
  const { t } = useTranslation();
  const toast = useToast();
  const { config, update } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const { getInstanceList } = useGlobalData();
  const instanceList = getInstanceList();
  const groups = config.states.instanceGroups;
  const navType = config.general.functionality.instancesNavType;
  const scrollRef = useRef<HTMLDivElement>(null);

  const [editing, setEditing] = useState<{
    sectionKey: string;
    instanceId: string;
  } | null>(null);
  const [editingGroup, setEditingGroup] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; id: string } | null>(
    null
  );
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const [pendingNewGroup, setPendingNewGroup] = useState<
    Omit<InstanceDrop, "target"> | undefined
  >();

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

  const submitGroupName = (id: string, name: string) => {
    setEditingGroup(null);
    const next = renameGroup(groups, id, name);
    if (next !== groups) update("states.instanceGroups", next);
    else if (
      name.trim() &&
      isGroupNameTaken(normalizeGroups(groups), name, id)
    ) {
      toast({
        title: t("InstanceGroups.newGroup.duplicate"),
        status: "warning",
      });
    }
  };

  const applyDrop = async (instanceId: string, result: DropResult) => {
    if (!sameGroups(result.groups, normalizeGroups(groups))) {
      await update("states.instanceGroups", result.groups);
    }
    const instance = instanceList?.find((item) => item.id === instanceId);
    if (
      result.starred !== undefined &&
      instance &&
      instance.starred !== result.starred
    ) {
      // the service serializes the value itself
      const response = await InstanceService.updateInstanceConfig(
        instanceId,
        "starred",
        result.starred
      );
      if (response.status === "success") getInstanceList(true);
      else
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
    }
  };

  const handleDropInstance = ({
    instanceId,
    source,
    target,
    copy,
  }: InstanceDrop) => {
    if (target === "new") {
      setPendingNewGroup({ instanceId, source, copy });
      return;
    }
    const result = dropInstance(groups, instanceId, source, target, copy);
    if (result) void applyDrop(instanceId, result);
  };

  const handleMoveSection = ({ key, targetKey, after }: SectionMove) => {
    const next = moveSection(groups, key, targetKey, after);
    if (next !== groups) update("states.instanceGroups", next);
  };

  const { drag, startDrag } = useSidebarDrag({
    scrollRef,
    onDropInstance: handleDropInstance,
    onMoveSection: handleMoveSection,
  });
  const draggingInstance = drag?.payload.type === "instance";
  const draggingSection = drag?.payload.type === "section";

  const previewBg = useColorModeValue("white", "gray.700");
  const lineColor = useColorModeValue(
    `${primaryColor}.500`,
    `${primaryColor}.300`
  );

  const accepts = (section: Section) =>
    drag?.payload.type === "instance" &&
    drag.target.type === "section" &&
    drag.target.section.key === section.key &&
    dropInstance(
      groups,
      drag.payload.instanceId,
      drag.payload.source,
      drag.target.section,
      drag.copy
    ) !== null;

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
      <Box flex="1" minH={0} overflowY="auto" ref={scrollRef}>
        {sections.map((section) => {
          // dragging a title shows every section folded, without changing the saved state
          const open = !draggingSection && !collapsed.includes(section.key);
          const sortable = isSortableSection(groups, section);
          const highlighted = accepts(section);
          return (
            <Box
              key={section.key}
              data-section-key={section.key}
              data-section-kind={section.kind}
              data-sortable={sortable}
              borderRadius="md"
              outline={highlighted ? "2px solid" : undefined}
              outlineColor={lineColor}
              outlineOffset="-2px"
              opacity={
                drag?.payload.type === "section" &&
                drag.payload.section.key === section.key
                  ? 0.5
                  : undefined
              }
            >
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
                onPointerDown={
                  sortable
                    ? (event) =>
                        startDrag(event, {
                          type: "section",
                          section: { key: section.key, kind: section.kind },
                          label: sectionTitle(section),
                        })
                    : undefined
                }
                editing={editingGroup === section.key}
                onStartEdit={
                  section.kind === "custom"
                    ? () => setEditingGroup(section.key)
                    : undefined
                }
                onSubmitName={(name) => submitGroupName(section.key, name)}
                onCancelEdit={() => setEditingGroup(null)}
                onContextMenu={
                  section.kind === "custom"
                    ? (event) => {
                        event.preventDefault();
                        setMenu({
                          x: event.clientX,
                          y: event.clientY,
                          id: section.key,
                        });
                      }
                    : undefined
                }
              />
              <Collapse
                in={open}
                animateOpacity
                transition={
                  draggingSection
                    ? { enter: { duration: 0 }, exit: { duration: 0 } }
                    : undefined
                }
              >
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
                      tooltipDisabled={!!drag}
                      onNavigate={() => navigateToInstance(instance)}
                      onStartEdit={() =>
                        setEditing({
                          sectionKey: section.key,
                          instanceId: instance.id,
                        })
                      }
                      onSubmitName={(name) => renameInstance(instance, name)}
                      onCancelEdit={() => setEditing(null)}
                      onPointerDown={(event) =>
                        startDrag(event, {
                          type: "instance",
                          instanceId: instance.id,
                          source: { key: section.key, kind: section.kind },
                          label: instance.name,
                        })
                      }
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
        {draggingInstance && (
          <Box
            data-sidebar-plus
            role="button"
            aria-label={t("InstanceGroups.newGroup.title")}
            mt={1}
            h={8}
            display="flex"
            alignItems="center"
            justifyContent="center"
            borderRadius="md"
            borderWidth="2px"
            borderStyle="dashed"
            borderColor={drag?.target.type === "new" ? lineColor : "gray.400"}
            bg={drag?.target.type === "new" ? "blackAlpha.100" : undefined}
          >
            <Icon as={LuPlus} />
          </Box>
        )}
      </Box>
      {drag && (
        <Box
          position="fixed"
          left={`${drag.x + 14}px`}
          top={`${drag.y + 10}px`}
          zIndex="tooltip"
          pointerEvents="none"
          maxW="240px"
          px={3}
          py={1}
          borderRadius="md"
          borderWidth="1px"
          boxShadow="md"
          bg={previewBg}
        >
          <Text fontSize="sm" className="ellipsis-text">
            {drag.copy && draggingInstance ? "+ " : ""}
            {drag.payload.label}
          </Text>
        </Box>
      )}
      {drag?.indicator && (
        <Box
          position="fixed"
          left={`${drag.indicator.left}px`}
          top={`${drag.indicator.top - 1}px`}
          w={`${drag.indicator.width}px`}
          h="2px"
          borderRadius="full"
          bg={lineColor}
          pointerEvents="none"
          zIndex="tooltip"
        />
      )}
      <Menu
        isOpen={!!menu}
        onClose={() => setMenu(null)}
        placement="bottom-start"
        isLazy
      >
        <MenuButton
          as={Box}
          position="fixed"
          left={`${menu?.x ?? 0}px`}
          top={`${menu?.y ?? 0}px`}
          w={0}
          h={0}
          aria-hidden
        />
        <MenuList>
          <MenuItem
            icon={<Icon as={groups.pinned === menu?.id ? LuPinOff : LuPin} />}
            onClick={() =>
              menu &&
              update("states.instanceGroups", togglePinned(groups, menu.id))
            }
          >
            {t(
              groups.pinned === menu?.id
                ? "InstanceGroups.menu.unpin"
                : "InstanceGroups.menu.pin"
            )}
          </MenuItem>
          <MenuItem
            icon={<Icon as={LuPencil} />}
            onClick={() => {
              if (!menu) return;
              const id = menu.id;
              // The menu gives focus back to its button when it closes, which
              // would blur the input at once; start editing after that.
              window.setTimeout(() => setEditingGroup(id), 100);
            }}
          >
            {t("InstanceGroups.menu.rename")}
          </MenuItem>
          <MenuItem
            icon={<Icon as={LuTrash2} />}
            color="red.500"
            onClick={() => menu && setPendingDelete(menu.id)}
          >
            {t("InstanceGroups.menu.delete")}
          </MenuItem>
        </MenuList>
      </Menu>
      <GenericConfirmDialog
        isOpen={pendingDelete !== null}
        onClose={() => setPendingDelete(null)}
        title={t("InstanceGroups.deleteGroup.title")}
        body={t("InstanceGroups.deleteGroup.body", {
          name: (pendingDelete && groups.custom[pendingDelete]?.name) || "",
        })}
        btnOK={t("General.delete")}
        onOKCallback={() => {
          if (pendingDelete) {
            update("states.instanceGroups", deleteGroup(groups, pendingDelete));
          }
        }}
        isAlert
      />
      <NewInstanceGroupModal
        isOpen={!!pendingNewGroup}
        onClose={() => setPendingNewGroup(undefined)}
        isNameTaken={(name) => isGroupNameTaken(normalizeGroups(groups), name)}
        onCreate={(name) => {
          if (!pendingNewGroup) return;
          const { instanceId, source, copy } = pendingNewGroup;
          const result = addToNewGroup(
            groups,
            newGroupId(),
            name,
            instanceId,
            source,
            copy
          );
          if (result) void applyDrop(instanceId, result);
        }}
      />
    </Stack>
  );
};

export default InstanceSidebar;
