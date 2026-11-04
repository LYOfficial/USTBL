import {
  Alert,
  AlertIcon,
  Badge,
  Box,
  Button,
  Collapse,
  HStack,
  Icon,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  ModalProps,
  Spinner,
  Text,
  Tooltip,
  VStack,
  useDisclosure,
} from "@chakra-ui/react";
import {
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { LuChevronDown, LuChevronRight } from "react-icons/lu";
import { DiffFile, DiffHunks, regionHunk } from "@/components/common/diff-view";
import {
  ChangeFilterButton,
  ChangeTag,
  FilePanelEmpty,
  FilePanelToolbar,
} from "@/components/common/file-panel";
import TreeView, { TreeCommand } from "@/components/common/tree-view";
import VirtualRows, {
  useViewportHeight,
} from "@/components/common/virtual-rows";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import {
  ModpackChangeDetail,
  ModpackChangeDetailEntry,
  ModpackIgnoredItem,
} from "@/models/modpack-update";
import { InvokeResponse } from "@/models/response";
import { VustbAccount, isVustbManager } from "@/models/vustb";
import { AccountService } from "@/services/account";
import { ModpackUpdateService } from "@/services/modpack-update";
import {
  AUTO_EXPAND_LIMIT,
  createPathMatcher,
  isQueryActive,
} from "@/utils/fuzzy-filter";
import {
  CLASS_ORDER,
  ChangeFilter,
  OPEN_LIMIT,
  buildChangeTree,
  countByClass,
  expandedDepth,
  filterEntries,
  isRuleDeleted,
  revealPath,
  sortEntriesByDiff,
  sortIgnoredByDiff,
  splitByGroup,
} from "@/utils/modpack-change-detail";
import { toTreeNodes } from "@/utils/path-tree";
import { formatByteSize } from "@/utils/string";

interface ModpackLocalChangesModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
  instanceName: string;
}

/** A titled section that folds, for the groups under the tree. */
const ChangeGroup: React.FC<{
  title: string;
  description: string;
  defaultOpen?: boolean;
  children: React.ReactNode;
}> = ({ title, description, defaultOpen = false, children }) => {
  const { isOpen, onToggle } = useDisclosure({ defaultIsOpen: defaultOpen });
  return (
    <VStack align="stretch" spacing={2}>
      <HStack spacing={2}>
        <HStack
          as="button"
          type="button"
          spacing={1}
          onClick={onToggle}
          aria-expanded={isOpen}
        >
          <Icon as={isOpen ? LuChevronDown : LuChevronRight} />
          <Text fontSize="xs-sm" fontWeight="medium">
            {title}
          </Text>
        </HStack>
      </HStack>
      <Collapse in={isOpen} animateOpacity={false}>
        <VStack align="stretch" spacing={2}>
          <Text fontSize="xs" className="secondary-text">
            {description}
          </Text>
          {children}
        </VStack>
      </Collapse>
    </VStack>
  );
};

/**
 * What differs here from the release the instance last matched, grouped by
 * directory and class (plan 《整合包本地改动明细》§5). A manager can go on to
 * publish.
 */
const ModpackLocalChangesModal: React.FC<ModpackLocalChangesModalProps> = ({
  instanceId,
  instanceName,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const toast = useToast();
  const { openSharedModal } = useSharedModals();
  const { modpackChecks, checkModpackUpdate, getInstanceList } =
    useGlobalData();

  const [detail, setDetail] = useState<ModpackChangeDetail | null>(null);
  const [failed, setFailed] = useState(false);
  const [ignored, setIgnored] = useState<ModpackIgnoredItem[]>([]);
  const [filter, setFilter] = useState<ChangeFilter | null>(null);
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [vustbAccount, setVustbAccount] = useState<VustbAccount | null>(null);

  const load = useCallback(
    async (keepShown = false) => {
      if (!keepShown) {
        setDetail(null);
        setFailed(false);
      }
      const [response, ignores] = await Promise.all([
        ModpackUpdateService.listModpackChangeDetail(instanceId),
        ModpackUpdateService.listModpackIgnores(instanceId),
      ]);
      if (response.status === "success") {
        setDetail(response.data);
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
        if (!keepShown) setFailed(true);
      }
      if (ignores.status === "success") setIgnored(ignores.data);
    },
    [instanceId, toast]
  );

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    AccountService.retrieveVustbAccount().then((response) => {
      if (response.status === "success") setVustbAccount(response.data);
    });
  }, []);

  // Keep the ledger's count in step with what is shown here.
  useEffect(() => {
    checkModpackUpdate(instanceId, true);
  }, [instanceId, checkModpackUpdate]);

  const revealFile = useCallback(
    async (path: string) => {
      const response = await ModpackUpdateService.revealModpackConflictFile(
        instanceId,
        path
      );
      if (response.status !== "success") {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    },
    [instanceId, toast]
  );

  /** Run an ignore or undo, then show the lists as they are now. */
  const changeIgnores = useCallback(
    async (action: () => Promise<InvokeResponse<null>>) => {
      setBusy(true);
      const response = await action();
      if (response.status !== "success") {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
      await load(true);
      // The ledger's count excludes ignored parts.
      checkModpackUpdate(instanceId, true);
      setBusy(false);
    },
    [instanceId, load, toast, checkModpackUpdate]
  );

  const entries = useMemo(() => detail?.entries ?? [], [detail]);
  const groups = useMemo(() => splitByGroup(entries), [entries]);
  // The counts and the filter cover the body only.
  const counts = useMemo(() => countByClass(groups.main), [groups.main]);
  // The list follows the filter one render behind, so typing and clicks stay quick.
  const shownFilter = useDeferredValue(filter);
  const shownQuery = useDeferredValue(query);
  const queryActive = isQueryActive(shownQuery);
  const matchesPath = useMemo(
    () => createPathMatcher(shownQuery),
    [shownQuery]
  );
  const shownMain = useMemo(
    () =>
      filterEntries(groups.main, shownFilter).filter((entry) =>
        matchesPath(entry.path)
      ),
    [groups.main, shownFilter, matchesPath]
  );
  const shownWillDelete = useMemo(
    () =>
      sortEntriesByDiff(
        groups.willDelete.filter((entry) => matchesPath(entry.path))
      ),
    [groups.willDelete, matchesPath]
  );
  const sortedIgnored = useMemo(() => sortIgnoredByDiff(ignored), [ignored]);
  // Hits open their parents without touching the remembered expansion, so
  // clearing the query brings back what the user had open.
  const expandAll = queryActive && shownMain.length <= AUTO_EXPAND_LIMIT;
  // TreeView resets its expansion whenever `nodes` changes identity.
  const tree = useMemo(
    () =>
      toTreeNodes(buildChangeTree(shownMain), (node) =>
        node.path.toLowerCase()
      ),
    [shownMain]
  );
  const openFiles = shownMain.length <= OPEN_LIMIT;
  // The tree draws only the cards in view, so which are open lives here.
  const [openCards, setOpenCards] = useState<Map<string, boolean>>(new Map());
  // What the cards without a choice of their own do; null follows the count.
  const [allCardsOpen, setAllCardsOpen] = useState<boolean | null>(null);
  const [command, setCommand] = useState<TreeCommand>();
  /** The two toolbar buttons: every directory and every card. */
  const openEverything = (open: boolean) => {
    setCommand({ type: open ? "expand" : "collapse" });
    setAllCardsOpen(open);
    setOpenCards(new Map());
  };
  const cardOpen = (path: string) =>
    openCards.get(path) ?? allCardsOpen ?? openFiles;
  // The tree scrolls inside the dialog, which scrolls for the groups below it.
  const treeHeight = useViewportHeight(0.55, 320);

  const isManager = isVustbManager(vustbAccount);
  // Publishing goes on top of the latest version; behind it the platform
  // refuses, so the player updates first.
  // A local import has no line to publish to.
  const isLocalImport =
    getInstanceList()?.find((instance) => instance.id === instanceId)?.origin
      ?.platform === "local";
  const behind =
    !isLocalImport && modpackChecks[instanceId]?.status === "available";
  const publishBlocker = behind
    ? t("ModpackLocalChangesModal.publishHint.updateFirst")
    : null;

  // A publisher also learns that ignoring leaves publishing alone.
  const hoverText = (key: string) =>
    isManager
      ? `${t(key)} ${t("ModpackLocalChangesModal.ignore.publishUnaffected")}`
      : t(key);

  const openPublish = () => {
    modalProps.onClose();
    openSharedModal("publish-modpack", { instanceId, instanceName });
  };

  const ignoreFileButton = (entry: ModpackChangeDetailEntry) => {
    const added = entry.class === "added";
    const baseSha1 = entry.baseline?.sha1;
    return (
      <Tooltip
        label={hoverText(
          added
            ? "ModpackLocalChangesModal.ignore.addedHint"
            : "ModpackLocalChangesModal.ignore.fileHint"
        )}
      >
        <Button
          size="xs"
          variant="ghost"
          isDisabled={busy || (!added && !baseSha1)}
          onClick={() =>
            changeIgnores(() =>
              added
                ? ModpackUpdateService.ignoreModpackAddedFile(
                    instanceId,
                    entry.path
                  )
                : ModpackUpdateService.ignoreModpackLocalChange(
                    instanceId,
                    entry.path,
                    baseSha1 ?? ""
                  )
            )
          }
        >
          {t(
            added
              ? "ModpackLocalChangesModal.ignore.added"
              : "ModpackLocalChangesModal.ignore.file"
          )}
        </Button>
      </Tooltip>
    );
  };

  const renderDiff = (entry: ModpackChangeDetailEntry) => {
    const note = (text: string) => (
      <Text fontSize="xs" className="secondary-text" px={3} py={2}>
        {text}
      </Text>
    );
    switch (entry.class) {
      case "modified":
        if (entry.hunks && entry.hunks.length > 0) {
          const baseSha1 = entry.baseline?.sha1;
          return (
            <DiffHunks
              hunks={entry.hunks}
              renderEditAction={(edit) => (
                <Tooltip
                  label={hoverText("ModpackLocalChangesModal.ignore.spotHint")}
                >
                  <Button
                    size="xs"
                    variant="outline"
                    isDisabled={busy || !baseSha1}
                    onClick={() =>
                      changeIgnores(() =>
                        ModpackUpdateService.ignoreModpackLocalChange(
                          instanceId,
                          entry.path,
                          baseSha1 ?? "",
                          { start: edit.start, end: edit.end }
                        )
                      )
                    }
                  >
                    {t("ModpackLocalChangesModal.ignore.spot")}
                  </Button>
                </Tooltip>
              )}
            />
          );
        }
        return note(
          t(`ModpackLocalChangesModal.noDiff.${entry.noDiff ?? "notText"}`)
        );
      default:
        return null;
    }
  };

  const renderBody = (entry: ModpackChangeDetailEntry) => (
    <>
      {/* What the next update does with this entry. */}
      {!isRuleDeleted(entry) && (
        <Text fontSize="xs" className="secondary-text" px={3} py={2}>
          {t(`ModpackLocalChangesModal.next.${entry.class}`)}
          {entry.size !== undefined &&
            entry.class === "added" &&
            ` (${formatByteSize(entry.size)})`}
        </Text>
      )}
      {renderDiff(entry)}
    </>
  );

  const renderEntry = (
    entry: ModpackChangeDetailEntry,
    label: string,
    withIgnore = true
  ) => {
    const reveal = revealPath(entry);
    const labelClass: ChangeFilter =
      entry.class === "modified"
        ? "modified"
        : entry.class === "toggled"
          ? entry.disabled
            ? "disable"
            : "enable"
          : entry.class === "addedExcluded"
            ? "added"
            : entry.class;
    return (
      <Box>
        <DiffFile
          path={label}
          hunks={entry.hunks}
          open={cardOpen(entry.path)}
          onOpenChange={(open) =>
            setOpenCards((prev) => new Map(prev).set(entry.path, open))
          }
          tags={
            <>
              <ChangeTag visual={labelClass} />
              {entry.class === "modified" && entry.toggled && (
                <ChangeTag visual={entry.disabled ? "disable" : "enable"} />
              )}
              {entry.disabled && !entry.toggled && (
                <Badge flexShrink={0}>
                  {t("ModpackLocalChangesModal.disabled")}
                </Badge>
              )}
            </>
          }
          actions={
            <>
              {reveal && (
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={() => revealFile(reveal)}
                >
                  {t("UpdateModpackModal.choice.revealFile")}
                </Button>
              )}
              {withIgnore && ignoreFileButton(entry)}
            </>
          }
        >
          {renderBody(entry)}
        </DiffFile>
      </Box>
    );
  };

  const renderIgnored = (item: ModpackIgnoredItem) => (
    <DiffFile
      key={item.path}
      path={item.path}
      defaultOpen={false}
      tags={
        <Badge flexShrink={0}>
          {item.added
            ? t("ModpackLocalChangesModal.ignored.addedFile")
            : item.whole
              ? t("ModpackLocalChangesModal.ignored.wholeFile")
              : t("ModpackLocalChangesModal.ignored.spots", {
                  count: item.regions.length,
                })}
        </Badge>
      }
      actions={
        <Tooltip label={hoverText("ModpackLocalChangesModal.ignored.undoHint")}>
          <Button
            size="xs"
            variant="ghost"
            isDisabled={busy}
            onClick={() =>
              changeIgnores(() =>
                ModpackUpdateService.unignoreModpackLocalChange(
                  instanceId,
                  item.path
                )
              )
            }
          >
            {t(
              item.added
                ? "ModpackLocalChangesModal.ignored.undo"
                : "ModpackLocalChangesModal.ignored.undoAll"
            )}
          </Button>
        </Tooltip>
      }
    >
      <Text fontSize="xs" className="secondary-text" px={3} py={2}>
        {t(
          item.added
            ? "ModpackLocalChangesModal.ignored.next.added"
            : item.whole
              ? "ModpackLocalChangesModal.ignored.next.whole"
              : "ModpackLocalChangesModal.ignored.next.spots",
          { count: item.regions.length }
        )}
      </Text>
      {!item.whole && item.regions.length > 0 && (
        <VStack align="stretch" spacing={0}>
          {item.regions.map((region, index) => (
            <Box
              key={`${region.start}-${region.end}`}
              borderTopWidth={index > 0 ? "1px" : 0}
            >
              <HStack justify="space-between" px={3} py={1.5} spacing={2}>
                <Text fontSize="xs" className="secondary-text">
                  {t("ModpackLocalChangesModal.ignored.lines", {
                    from: region.start + 1,
                    to: Math.max(region.end, region.start + 1),
                  })}
                </Text>
                <Tooltip
                  label={hoverText("ModpackLocalChangesModal.ignored.undoHint")}
                >
                  <Button
                    size="xs"
                    variant="outline"
                    isDisabled={busy}
                    onClick={() =>
                      changeIgnores(() =>
                        ModpackUpdateService.unignoreModpackLocalChange(
                          instanceId,
                          item.path,
                          { start: region.start, end: region.end }
                        )
                      )
                    }
                  >
                    {t("ModpackLocalChangesModal.ignored.undo")}
                  </Button>
                </Tooltip>
              </HStack>
              <DiffHunks
                hideHunkHeaders
                hunks={[
                  regionHunk(
                    region.start + 1,
                    region.baseLines,
                    region.localLines
                  ),
                ]}
              />
            </Box>
          ))}
        </VStack>
      )}
    </DiffFile>
  );

  return (
    <Modal
      scrollBehavior="inside"
      size={{ base: "2xl", lg: "4xl" }}
      autoFocus={false}
      {...modalProps}
    >
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>
          {t("ModpackLocalChangesModal.header.title", { name: instanceName })}
        </ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          {detail === null ? (
            failed ? null : (
              <VStack py={6} spacing={2}>
                <Spinner size="sm" />
                <Text fontSize="xs" className="secondary-text">
                  {t("ModpackLocalChangesModal.loading")}
                </Text>
              </VStack>
            )
          ) : (
            <VStack align="stretch" spacing={3}>
              <VStack align="stretch" spacing={1}>
                <Text fontSize="xs" className="secondary-text">
                  {t("ModpackLocalChangesModal.comparedTo", {
                    label: detail.label,
                  })}
                </Text>
                {detail.incomplete && (
                  <Alert status="warning" fontSize="xs-sm">
                    <AlertIcon />
                    {t("ModpackLocalChangesModal.incomplete")}
                  </Alert>
                )}
                {groups.main.length === 0 ? (
                  groups.willDelete.length === 0 && (
                    <Text className="secondary-text">
                      {t("ModpackLocalChangesModal.empty")}
                    </Text>
                  )
                ) : (
                  <>
                    <FilePanelToolbar
                      query={query}
                      onQueryChange={setQuery}
                      onExpandAll={() => openEverything(true)}
                      onCollapseAll={() => openEverything(false)}
                      controls={CLASS_ORDER.filter(
                        (name) => counts[name] > 0
                      ).map((name) => (
                        <ChangeFilterButton
                          key={name}
                          visual={name}
                          count={counts[name]}
                          selected={filter === name}
                          onClick={() =>
                            setFilter((prev) => (prev === name ? null : name))
                          }
                        />
                      ))}
                    />
                    <Text fontSize="xs" className="secondary-text">
                      {t("ModpackLocalChangesModal.countingRule")}
                    </Text>
                  </>
                )}
              </VStack>
              {tree.length > 0 && (
                <TreeView
                  nodes={tree}
                  defaultExpandedDepth={expandedDepth(shownMain.length)}
                  preserveExpanded
                  expandAll={expandAll}
                  command={command}
                  maxHeight={treeHeight}
                  leafPlaceholderWidth={0}
                  renderNode={({ node: { data } }) =>
                    data.entry ? (
                      renderEntry(data.entry, data.name)
                    ) : (
                      <HStack spacing={2}>
                        <Text fontSize="xs-sm" fontWeight="medium">
                          {data.name}
                        </Text>
                        <Badge flexShrink={0}>{data.count}</Badge>
                      </HStack>
                    )
                  }
                />
              )}
              {tree.length === 0 &&
                shownWillDelete.length === 0 &&
                queryActive && <FilePanelEmpty filtered />}
              {shownWillDelete.length > 0 && (
                <ChangeGroup
                  title={t("ModpackLocalChangesModal.willDelete.title", {
                    count: shownWillDelete.length,
                  })}
                  description={t(
                    "ModpackLocalChangesModal.willDelete.description"
                  )}
                  defaultOpen
                >
                  <VirtualRows
                    rowCount={shownWillDelete.length}
                    rowKey={(index) => shownWillDelete[index].path}
                    renderRow={(index) => (
                      <Box pb={2}>
                        {renderEntry(
                          shownWillDelete[index],
                          shownWillDelete[index].path,
                          false
                        )}
                      </Box>
                    )}
                    layoutKey={shownWillDelete}
                    maxHeight={treeHeight}
                  />
                </ChangeGroup>
              )}
            </VStack>
          )}
          {detail !== null && ignored.length > 0 && (
            <Box mt={4}>
              <ChangeGroup
                title={t("ModpackLocalChangesModal.ignored.title", {
                  count: ignored.length,
                })}
                description={t("ModpackLocalChangesModal.ignored.description")}
              >
                {sortedIgnored.map(renderIgnored)}
              </ChangeGroup>
            </Box>
          )}
          {behind && isManager && entries.length > 0 && (
            <Alert status="info" mt={3} fontSize="xs-sm">
              <AlertIcon />
              {t("ModpackLocalChangesModal.publishHint.updateFirst")}
            </Alert>
          )}
        </ModalBody>
        <ModalFooter>
          <Button variant="ghost" mr={2} onClick={modalProps.onClose}>
            {t("General.close")}
          </Button>
          {isManager && !isLocalImport && (
            <Tooltip label={publishBlocker} isDisabled={!publishBlocker}>
              {/* A disabled button fires no hover, so the box carries the tooltip. */}
              <Box>
                <Button
                  colorScheme={primaryColor}
                  isDisabled={!!publishBlocker || entries.length === 0}
                  onClick={openPublish}
                >
                  {t("ModpackLocalChangesModal.button.publish")}
                </Button>
              </Box>
            </Tooltip>
          )}
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default ModpackLocalChangesModal;
