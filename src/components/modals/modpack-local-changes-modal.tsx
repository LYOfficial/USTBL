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
  Wrap,
  WrapItem,
  useDisclosure,
} from "@chakra-ui/react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuChevronDown, LuChevronRight } from "react-icons/lu";
import { DiffFile, DiffHunks, regionHunk } from "@/components/common/diff-view";
import TreeView, { TreeNode } from "@/components/common/tree-view";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import {
  ModpackChangeClass,
  ModpackChangeDetail,
  ModpackChangeDetailEntry,
  ModpackIgnoredItem,
} from "@/models/modpack-update";
import { InvokeResponse } from "@/models/response";
import { VustbAccount, isVustbManager } from "@/models/vustb";
import { AccountService } from "@/services/account";
import { ModpackUpdateService } from "@/services/modpack-update";
import {
  CLASS_ORDER,
  ChangeTreeNode,
  OPEN_LIMIT,
  buildChangeTree,
  countByClass,
  countedTotal,
  expandedDepth,
  filterEntries,
  revealPath,
  splitByGroup,
} from "@/utils/modpack-change-detail";
import { formatByteSize } from "@/utils/string";

interface ModpackLocalChangesModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
  instanceName: string;
}

const CLASS_COLORS: Record<ModpackChangeClass, string> = {
  modified: "orange",
  deleted: "red",
  toggled: "gray",
  onceChanged: "purple",
  added: "green",
  addedExcluded: "gray",
};

/** A titled section that folds, for the groups under the tree. */
const ChangeGroup: React.FC<{
  title: string;
  tags?: React.ReactNode;
  description: string;
  defaultOpen?: boolean;
  children: React.ReactNode;
}> = ({ title, tags, description, defaultOpen = false, children }) => {
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
        {tags}
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
  const { modpackChecks, checkModpackUpdate } = useGlobalData();

  const [detail, setDetail] = useState<ModpackChangeDetail | null>(null);
  const [failed, setFailed] = useState(false);
  const [ignored, setIgnored] = useState<ModpackIgnoredItem[]>([]);
  const [filter, setFilter] = useState<ModpackChangeClass | null>(null);
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
  const counts = useMemo(() => countByClass(entries), [entries]);
  const shown = useMemo(
    () => splitByGroup(filterEntries(entries, filter)),
    [entries, filter]
  );
  // TreeView resets its expansion whenever `nodes` changes identity.
  const tree = useMemo(() => {
    const convert = (list: ChangeTreeNode[]): TreeNode<ChangeTreeNode>[] =>
      list.map((node) => ({
        id: node.path.toLowerCase(),
        data: node,
        children: convert(node.children),
      }));
    return convert(buildChangeTree(shown.main));
  }, [shown.main]);
  const openFiles = shown.main.length <= OPEN_LIMIT;

  const isManager = isVustbManager(vustbAccount);
  // Publishing goes on top of the latest version; behind it the platform
  // refuses, so the player updates first.
  const behind = modpackChecks[instanceId]?.status === "available";
  const publishBlocker = !isManager
    ? t("ModpackLocalChangesModal.publishHint.noPermission")
    : behind
      ? t("ModpackLocalChangesModal.publishHint.updateFirst")
      : null;

  const openPublish = () => {
    modalProps.onClose();
    openSharedModal("publish-modpack", { instanceId, instanceName });
  };

  const ignoreFileButton = (entry: ModpackChangeDetailEntry) => {
    const added = entry.class === "added" || entry.class === "addedExcluded";
    const baseSha1 = entry.baseline?.sha1;
    return (
      <Tooltip
        label={t(
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

  const renderBody = (entry: ModpackChangeDetailEntry) => {
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
                <Tooltip label={t("ModpackLocalChangesModal.ignore.spotHint")}>
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
      case "added":
      case "addedExcluded":
        return entry.size !== undefined
          ? note(
              t("ModpackLocalChangesModal.hint.added", {
                size: formatByteSize(entry.size),
              })
            )
          : null;
      default:
        return note(t(`ModpackLocalChangesModal.hint.${entry.class}`));
    }
  };

  const renderEntry = (entry: ModpackChangeDetailEntry, label: string) => {
    const reveal = revealPath(entry);
    return (
      <Box opacity={entry.counted ? 1 : 0.75}>
        <DiffFile
          path={label}
          hunks={entry.hunks}
          defaultOpen={openFiles}
          tags={
            <>
              <Badge colorScheme={CLASS_COLORS[entry.class]} flexShrink={0}>
                {t(`ModpackLocalChangesModal.class.${entry.class}`)}
              </Badge>
              {entry.class === "modified" && entry.toggled && (
                <Badge flexShrink={0}>
                  {t("ModpackLocalChangesModal.toggledTag")}
                </Badge>
              )}
              {entry.disabled && (
                <Badge flexShrink={0}>
                  {t("ModpackLocalChangesModal.disabled")}
                </Badge>
              )}
              {!entry.counted && (
                <Badge variant="outline" flexShrink={0}>
                  {t("ModpackLocalChangesModal.notCounted")}
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
              {entry.class !== "onceChanged" && ignoreFileButton(entry)}
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
        <Tooltip
          label={t(
            item.added
              ? "ModpackLocalChangesModal.ignored.undoAddedHint"
              : "ModpackLocalChangesModal.ignored.undoAllHint"
          )}
        >
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
                <Tooltip label={t("ModpackLocalChangesModal.ignored.undoHint")}>
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

  const showExcluded =
    shown.excluded.length > 0 &&
    (filter === null || filter === "addedExcluded");

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
                {entries.length === 0 ? (
                  <Text className="secondary-text">
                    {t("ModpackLocalChangesModal.empty")}
                  </Text>
                ) : (
                  <>
                    <Text fontSize="xs-sm">
                      {t("ModpackLocalChangesModal.summary", {
                        count: countedTotal(entries),
                      })}
                    </Text>
                    <Wrap spacing={1.5} pt={1}>
                      {CLASS_ORDER.filter((name) => counts[name] > 0).map(
                        (name) => (
                          <WrapItem key={name}>
                            <Button
                              size="xs"
                              colorScheme={CLASS_COLORS[name]}
                              variant={filter === name ? "solid" : "outline"}
                              aria-pressed={filter === name}
                              onClick={() =>
                                setFilter((prev) =>
                                  prev === name ? null : name
                                )
                              }
                            >
                              {t(`ModpackLocalChangesModal.class.${name}`)}{" "}
                              {counts[name]}
                            </Button>
                          </WrapItem>
                        )
                      )}
                    </Wrap>
                    <Text fontSize="xs" className="secondary-text">
                      {t("ModpackLocalChangesModal.filterHint")}{" "}
                      {t("ModpackLocalChangesModal.countingRule")}
                    </Text>
                  </>
                )}
              </VStack>
              {tree.length > 0 && (
                <TreeView
                  nodes={tree}
                  defaultExpandedDepth={expandedDepth(shown.main.length)}
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
              {showExcluded && (
                <ChangeGroup
                  key={`excluded-${filter}`}
                  title={t("ModpackLocalChangesModal.excluded.title", {
                    count: shown.excluded.length,
                  })}
                  description={t(
                    "ModpackLocalChangesModal.excluded.description"
                  )}
                  defaultOpen={filter === "addedExcluded"}
                >
                  {shown.excluded.map((entry) => (
                    <Box key={entry.path}>{renderEntry(entry, entry.path)}</Box>
                  ))}
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
                tags={
                  isManager && (
                    <Tooltip
                      label={t(
                        "ModpackLocalChangesModal.ignored.stillPublishedHint"
                      )}
                    >
                      <Badge colorScheme="orange" flexShrink={0}>
                        {t("ModpackLocalChangesModal.ignored.stillPublished")}
                      </Badge>
                    </Tooltip>
                  )
                }
                description={t("ModpackLocalChangesModal.ignored.description")}
              >
                {ignored.map(renderIgnored)}
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
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default ModpackLocalChangesModal;
