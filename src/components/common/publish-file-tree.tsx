import {
  Box,
  Button,
  Center,
  Checkbox,
  HStack,
  IconButton,
  Tag,
  TagLabel,
  TagLeftIcon,
  Text,
  Tooltip,
  VStack,
} from "@chakra-ui/react";
import { useDeferredValue, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  LuBan,
  LuCheck,
  LuFileDiff,
  LuMinus,
  LuTrash2,
  LuTriangleAlert,
  LuUndo2,
  LuX,
} from "react-icons/lu";
import { BeatLoader } from "react-spinners";
import BlockToggle from "@/components/common/block-toggle";
import {
  ChangeFilterButton,
  ChangeTag,
  FilePanelEmpty,
  FilePanelToolbar,
} from "@/components/common/file-panel";
import PublishChangeDiff from "@/components/common/publish-change-diff";
import TreeView, { TreeCommand } from "@/components/common/tree-view";
import { useLauncherConfig } from "@/contexts/config";
import {
  AUTO_EXPAND_LIMIT,
  countFiles,
  filterTreeByQuery,
  isQueryActive,
} from "@/utils/fuzzy-filter";
import {
  CHANGE_CATEGORIES,
  ChangeCategory,
  ChangeFilter,
  NO_FILTER,
  OnceCount,
  PreviousRelease,
  PublishChange,
  PublishContext,
  PublishDraft,
  PublishNode,
  ShownState,
  buildPublishNodes,
  canRemoveFile,
  categorizeChanges,
  countCategories,
  countOnceByDir,
  dirChangeCounts,
  filterNodes,
  isFilterActive,
  keyOf,
  localDeletePaths,
  nextState,
  onceStateUnder,
  restoreInheritance,
  setDirState,
  setFileState,
  setOnce,
  setOnceUnder,
  toggleCategory,
  viewOf,
} from "@/utils/modpack-publish-tree";
import { toTreeNodes } from "@/utils/path-tree";
import { branchIds } from "@/utils/tree-expansion";

const STATE_STYLE: Record<
  ShownState,
  { colorScheme: string; icon: React.ReactElement }
> = {
  include: { colorScheme: "blue", icon: <LuCheck /> },
  ignore: { colorScheme: "gray", icon: <LuBan /> },
  delete: { colorScheme: "red", icon: <LuTrash2 /> },
  remove: { colorScheme: "gray", icon: <LuX /> },
  mixed: { colorScheme: "purple", icon: <LuMinus /> },
};

interface PublishFileTreeProps {
  instanceId: string;
  context: PublishContext;
  draft: PublishDraft;
  /** How the files differ from the previous release. */
  changes: PublishChange[];
  /** The previous release; null when there is none to compare with. */
  previous: PreviousRelease | null;
  isLoading: boolean;
  onDraftChange: (updater: (prev: PublishDraft) => PublishDraft) => void;
}

/**
 * Tri-state tree for choosing what a modpack version publishes: each node is
 * included, ignored or deleted. The state is shown as an icon and a label, and
 * the button that changes it works from the keyboard. Files that differ from
 * the previous release carry a mark, which can filter the tree.
 */
const PublishFileTree: React.FC<PublishFileTreeProps> = ({
  instanceId,
  context,
  draft,
  changes,
  previous,
  isLoading,
  onDraftChange,
}) => {
  const { t } = useTranslation();
  const [filter, setFilter] = useState<ChangeFilter>(NO_FILTER);
  const nodes = useMemo(() => buildPublishNodes(context), [context]);
  const view = useMemo(() => viewOf(context, draft), [context, draft]);
  const marks = useMemo(
    () =>
      categorizeChanges(
        changes,
        context.ghosts,
        previous,
        view,
        new Map(context.files.map((file) => [keyOf(file.path), file.disabled]))
      ),
    [changes, context, previous, view]
  );
  const counts = useMemo(() => countCategories(marks), [marks]);
  const onceCounts = useMemo(
    () => countOnceByDir(context, view),
    [context, view]
  );
  const dirCounts = useMemo(() => dirChangeCounts(marks), [marks]);
  const localDeletes = useMemo(
    () => new Set(localDeletePaths(context, draft)),
    [context, draft]
  );
  const localDeleteDirCounts = useMemo(() => {
    const counts = new Map<string, number>();
    localDeletes.forEach((path) => {
      let cut = path.lastIndexOf("/");
      while (cut > 0) {
        const parent = keyOf(path.slice(0, cut));
        counts.set(parent, (counts.get(parent) ?? 0) + 1);
        cut = path.lastIndexOf("/", cut - 1);
      }
    });
    return counts;
  }, [localDeletes]);
  const changeByKey = useMemo(
    () => new Map(changes.map((change) => [keyOf(change.path), change])),
    [changes]
  );
  const [query, setQuery] = useState("");
  // Filtering thousands of files must not hold up typing or a click: the tree
  // follows these one render behind, and the controls answer at once.
  const shownFilter = useDeferredValue(filter);
  const shownQuery = useDeferredValue(query);
  const filtered = isFilterActive(shownFilter);
  const [command, setCommand] = useState<TreeCommand>();
  const [openDiffs, setOpenDiffs] = useState<Set<string>>(new Set());
  const toggleDiff = (path: string) =>
    setOpenDiffs((prev) => {
      const next = new Set(prev);
      if (!next.delete(path)) next.add(path);
      return next;
    });
  const queryActive = isQueryActive(shownQuery);
  const markedNodes = useMemo(
    () => filterNodes(nodes, marks, shownFilter),
    [nodes, marks, shownFilter]
  );
  const shownNodes = useMemo(
    () => filterTreeByQuery(markedNodes, shownQuery),
    [markedNodes, shownQuery]
  );
  const tree = useMemo(
    () => toTreeNodes(shownNodes, (node) => node.path),
    [shownNodes]
  );
  // Hits open their parents without touching the remembered expansion, so
  // clearing the query brings back what the user had open.
  const expandAll = useMemo(
    () => queryActive && countFiles(shownNodes) <= AUTO_EXPAND_LIMIT,
    [queryActive, shownNodes]
  );
  // The request must change only when the filter changes, not when draft state rebuilds the tree.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const expandRequest = useMemo(
    () => (filtered ? { ids: branchIds(tree) } : undefined),
    // The request must change only when the filter changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [shownFilter]
  );

  const label = (state: ShownState) => t(`PublishModpackModal.tree.${state}`);

  return (
    <Box w="100%">
      {!isLoading && (
        <VStack align="stretch" spacing={2} mb={2}>
          <FilePanelToolbar
            query={query}
            onQueryChange={setQuery}
            onExpandAll={() => setCommand({ type: "expand" })}
            onCollapseAll={() => setCommand({ type: "collapse" })}
            controls={
              <>
                <Text fontSize="xs" className="secondary-text">
                  {t("PublishModpackModal.tree.filter.title")}
                </Text>
                {CHANGE_CATEGORIES.map((category) => (
                  <Tooltip
                    key={category}
                    label={t(`PublishModpackModal.tree.changeHint.${category}`)}
                  >
                    <span>
                      <ChangeFilterButton
                        visual={category}
                        count={counts[category]}
                        selected={filter.categories.has(category)}
                        isDisabled={counts[category] === 0}
                        onClick={() =>
                          setFilter((prev) => toggleCategory(prev, category))
                        }
                      />
                    </span>
                  </Tooltip>
                ))}
                <BlockToggle
                  checked={filter.onlyChanged}
                  onChange={(checked) =>
                    setFilter((prev) => ({ ...prev, onlyChanged: checked }))
                  }
                  aria-label={t("PublishModpackModal.tree.filter.onlyChanged")}
                >
                  {t("PublishModpackModal.tree.filter.onlyChanged")}
                </BlockToggle>
              </>
            }
          />
          {previous === null && (
            <Text fontSize="xs" className="secondary-text">
              {t("PublishModpackModal.tree.noPrevious")}
            </Text>
          )}
        </VStack>
      )}
      <HStack
        fontSize="xs"
        className="secondary-text"
        mb={2}
        spacing={3}
        flexWrap="wrap"
      >
        {(["include", "ignore", "delete"] as const).map((state) => (
          <HStack key={state} spacing={1}>
            <Box color={`${STATE_STYLE[state].colorScheme}.500`}>
              {STATE_STYLE[state].icon}
            </Box>
            <Text fontWeight="medium">
              {t(`PublishModpackModal.tree.${state}`)}:{" "}
              {t(`PublishModpackModal.tree.legend.${state}`)}
            </Text>
          </HStack>
        ))}
        <Text w="100%">{t("PublishModpackModal.tree.legend.cycle")}</Text>
      </HStack>
      <Box>
        {isLoading ? (
          <Center py={4}>
            <BeatLoader size={10} color="gray" />
          </Center>
        ) : tree.length > 0 ? (
          <TreeView
            nodes={tree}
            defaultExpandedDepth={0}
            preserveExpanded
            expandRequest={expandRequest}
            expandAll={expandAll}
            command={command}
            renderNode={({ node: { data } }) => (
              <PublishTreeRow
                instanceId={instanceId}
                data={data}
                context={context}
                draft={draft}
                view={view}
                onceCounts={onceCounts}
                label={label}
                category={marks.get(keyOf(data.path))}
                change={changeByKey.get(keyOf(data.path))}
                dirChanges={dirCounts.get(keyOf(data.path)) ?? 0}
                localDeleteCount={
                  localDeleteDirCounts.get(keyOf(data.path)) ?? 0
                }
                showDiff={openDiffs.has(data.path)}
                onToggleDiff={() => toggleDiff(data.path)}
                onDraftChange={onDraftChange}
              />
            )}
          />
        ) : (
          <FilePanelEmpty filtered={queryActive || filtered} />
        )}
      </Box>
    </Box>
  );
};

interface PublishTreeRowProps {
  instanceId: string;
  data: PublishNode;
  context: PublishContext;
  draft: PublishDraft;
  view: ReturnType<typeof viewOf>;
  onceCounts: ReadonlyMap<string, OnceCount>;
  label: (state: ShownState) => string;
  category: ChangeCategory | undefined;
  change: PublishChange | undefined;
  dirChanges: number;
  localDeleteCount: number;
  /** Held by the tree: a row is destroyed when it scrolls out of view. */
  showDiff: boolean;
  onToggleDiff: () => void;
  onDraftChange: (updater: (prev: PublishDraft) => PublishDraft) => void;
}

const PublishTreeRow: React.FC<PublishTreeRowProps> = ({
  instanceId,
  data,
  context,
  draft,
  view,
  onceCounts,
  label,
  category,
  change,
  dirChanges,
  localDeleteCount,
  showDiff,
  onToggleDiff,
  onDraftChange,
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;

  const isDir = data.kind === "dir";
  const state = isDir ? view.dirState(data.path) : view.fileState(data.path);
  const canRemove = data.ghost && (isDir || canRemoveFile(draft, data.path));
  const next = nextState(state, { canRemove, ghost: data.ghost });
  const style = STATE_STYLE[state];
  const apply = () =>
    onDraftChange((prev) =>
      !isDir
        ? setFileState(context, prev, data.path, next)
        : next === "remove"
          ? restoreInheritance(prev, data.path, "dir")
          : setDirState(context, prev, data.path, next)
    );
  const description = t("PublishModpackModal.tree.toggle", {
    name: data.name,
    state: label(state),
    next: label(next),
  });
  // A diff is only worth drawing for a file the previous release has.
  const canDiff =
    change !== undefined &&
    (change.kind === "modified" || change.kind === "onceChanged") &&
    change.previousSha1 !== null;

  // Files and directories share one small box in a fixed-width slot at the
  // row's right edge, so the boxes line up whether or not a row has one.
  const dirOnce = isDir ? onceStateUnder(onceCounts, data.path) : null;
  let onceControl: React.ReactNode = null;
  if (!isDir && !data.ghost && state === "include") {
    onceControl = (
      <Tooltip
        label={
          <>
            <Text>{t("PublishModpackModal.tree.once")}</Text>
            <Text mt={1} opacity={0.8}>
              {t("PublishModpackModal.tree.onceHint")}
            </Text>
          </>
        }
      >
        <span>
          <Checkbox
            size="sm"
            colorScheme={primaryColor}
            isChecked={view.isOnce(data.path)}
            onChange={(e) =>
              onDraftChange((prev) =>
                setOnce(prev, data.path, e.target.checked)
              )
            }
            aria-label={`${data.name}: ${t("PublishModpackModal.tree.once")}`}
          />
        </span>
      </Tooltip>
    );
  } else if (dirOnce !== null) {
    onceControl = (
      <Tooltip
        label={
          <>
            <Text>{t("PublishModpackModal.tree.onceDir")}</Text>
            <Text mt={1} opacity={0.8}>
              {t("PublishModpackModal.tree.onceHint")}
            </Text>
          </>
        }
      >
        <span>
          <Checkbox
            size="sm"
            colorScheme={primaryColor}
            isChecked={dirOnce === "all"}
            isIndeterminate={dirOnce === "some"}
            onChange={() =>
              onDraftChange((prev) =>
                setOnceUnder(
                  context,
                  prev,
                  viewOf(context, prev),
                  data.path,
                  dirOnce !== "all"
                )
              )
            }
            aria-label={`${data.name}: ${t("PublishModpackModal.tree.onceDir")}`}
          />
        </span>
      </Tooltip>
    );
  }

  return (
    <VStack align="stretch" spacing={1} w="100%">
      <HStack spacing={2} fontSize="xs-sm" w="100%">
        <Tooltip
          label={
            <>
              <Text>{t(`PublishModpackModal.tree.hint.${state}`)}</Text>
              <Text mt={1} opacity={0.8}>
                {t("PublishModpackModal.tree.clickTo", {
                  next: label(next),
                })}
              </Text>
            </>
          }
        >
          <IconButton
            size="xs"
            variant={state === "include" ? "solid" : "outline"}
            colorScheme={style.colorScheme}
            icon={style.icon}
            minW="0"
            px={2}
            flexShrink={0}
            aria-label={description}
            onClick={apply}
          />
        </Tooltip>
        <Text
          className="ellipsis-text"
          opacity={data.ghost ? 0.7 : 1}
          fontStyle={data.ghost ? "italic" : undefined}
        >
          {data.name}
        </Text>
        {data.disabled && category !== "enable" && category !== "disable" ? (
          <Tag size="sm" flexShrink={0}>
            <TagLabel>{t("PublishModpackModal.tree.disabled")}</TagLabel>
          </Tag>
        ) : null}
        {data.ghost ? (
          <Tag size="sm" flexShrink={0}>
            <TagLabel>{t("PublishModpackModal.tree.missing")}</TagLabel>
          </Tag>
        ) : null}
        {data.conflict ? (
          <Tag size="sm" colorScheme="orange" flexShrink={0}>
            <TagLeftIcon as={LuTriangleAlert} />
            <TagLabel>{t("PublishModpackModal.tree.conflict")}</TagLabel>
          </Tag>
        ) : null}
        {!isDir && !data.ghost && state === "delete" ? (
          <Tooltip label={t("PublishModpackModal.tree.localDeleteHint")}>
            <Tag size="sm" colorScheme="red" flexShrink={0}>
              <TagLabel>{t("PublishModpackModal.tree.localDelete")}</TagLabel>
            </Tag>
          </Tooltip>
        ) : null}
        {!isDir && category ? (
          <Tooltip
            label={t(
              `PublishModpackModal.tree.changeHint.${
                category === "enable" || category === "disable"
                  ? "toggled"
                  : category
              }`
            )}
          >
            <span>
              <ChangeTag visual={category} />
            </span>
          </Tooltip>
        ) : null}
        {!isDir && change?.localIgnored ? (
          <Tooltip label={t("PublishModpackModal.tree.localIgnoredHint")}>
            <Tag size="sm" flexShrink={0}>
              <TagLabel>{t("PublishModpackModal.tree.localIgnored")}</TagLabel>
            </Tag>
          </Tooltip>
        ) : null}
        {isDir && dirChanges > 0 ? (
          <Tag size="sm" variant="subtle" flexShrink={0}>
            <TagLabel>
              {t("PublishModpackModal.tree.dirChanges", { count: dirChanges })}
            </TagLabel>
          </Tag>
        ) : null}
        {isDir && localDeleteCount > 0 ? (
          <Tag size="sm" colorScheme="red" flexShrink={0}>
            <TagLabel>
              {t("PublishModpackModal.tree.localDeleteCount", {
                count: localDeleteCount,
              })}
            </TagLabel>
          </Tag>
        ) : null}
        <Box flex="1" />
        {canDiff ? (
          <Button
            size="xs"
            variant="ghost"
            leftIcon={<LuFileDiff />}
            flexShrink={0}
            aria-expanded={showDiff}
            onClick={onToggleDiff}
          >
            {t(
              showDiff
                ? "PublishModpackModal.tree.diff.hide"
                : "PublishModpackModal.tree.diff.show"
            )}
          </Button>
        ) : null}
        {view.hasOverride(data.path, data.kind) ? (
          <Tooltip label={t("PublishModpackModal.tree.restoreHint")}>
            <IconButton
              size="xs"
              variant="ghost"
              icon={<LuUndo2 />}
              aria-label={`${data.name}: ${t("PublishModpackModal.tree.restore")}`}
              onClick={() =>
                onDraftChange((prev) =>
                  restoreInheritance(prev, data.path, data.kind)
                )
              }
            />
          </Tooltip>
        ) : null}
        <Box
          w="20px"
          h="20px"
          flexShrink={0}
          display="flex"
          alignItems="center"
          justifyContent="center"
        >
          {onceControl}
        </Box>
      </HStack>
      {canDiff && showDiff && change ? (
        <Box pl={2} pb={1}>
          <PublishChangeDiff instanceId={instanceId} change={change} />
        </Box>
      ) : null}
    </VStack>
  );
};

export default PublishFileTree;
