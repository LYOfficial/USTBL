import {
  Box,
  Button,
  Center,
  HStack,
  IconButton,
  Tag,
  TagLabel,
  TagLeftIcon,
  Text,
  Tooltip,
  VStack,
} from "@chakra-ui/react";
import { useMemo, useState } from "react";
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
import Empty from "@/components/common/empty";
import PublishChangeDiff from "@/components/common/publish-change-diff";
import TreeView, { TreeNode } from "@/components/common/tree-view";
import {
  CHANGE_CATEGORIES,
  ChangeCategory,
  ChangeFilter,
  NO_FILTER,
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
  dirChangeCounts,
  filterNodes,
  isFilterActive,
  keyOf,
  localDeletePaths,
  nextState,
  restoreInheritance,
  setDirState,
  setFileState,
  setOnce,
  toggleCategory,
  viewOf,
} from "@/utils/modpack-publish-tree";
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

const CHANGE_COLOR: Record<ChangeCategory, string> = {
  modified: "orange",
  added: "green",
  enable: "purple",
  disable: "purple",
  onceChanged: "cyan",
  deleted: "red",
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
  const filtered = isFilterActive(filter);
  const tree = useMemo(() => {
    const convert = (list: PublishNode[]): TreeNode<PublishNode>[] =>
      list.map((node) => ({
        id: node.path,
        data: node,
        children: convert(node.children),
      }));
    return convert(filterNodes(nodes, marks, filter));
  }, [nodes, marks, filter]);
  // The request must change only when the filter changes, not when draft state rebuilds the tree.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const expandRequest = useMemo(
    () => (filtered ? { ids: branchIds(tree) } : undefined),
    // The request must change only when the filter changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [filter]
  );

  const label = (state: ShownState) => t(`PublishModpackModal.tree.${state}`);

  return (
    <Box w="100%">
      {!isLoading && (
        <VStack align="stretch" spacing={2} mb={2}>
          <HStack spacing={2} flexWrap="wrap">
            <Text fontSize="xs" className="secondary-text">
              {t("PublishModpackModal.tree.filter.title")}
            </Text>
            {CHANGE_CATEGORIES.map((category) => (
              <Tooltip
                key={category}
                label={t(`PublishModpackModal.tree.changeHint.${category}`)}
              >
                <Button
                  size="xs"
                  colorScheme={CHANGE_COLOR[category]}
                  variant={
                    filter.categories.has(category) ? "solid" : "outline"
                  }
                  isDisabled={counts[category] === 0}
                  aria-pressed={filter.categories.has(category)}
                  onClick={() =>
                    setFilter((prev) => toggleCategory(prev, category))
                  }
                >
                  {t(`PublishModpackModal.tree.change.${category}`)}{" "}
                  {counts[category]}
                </Button>
              </Tooltip>
            ))}
            <Box flex="1" />
            <BlockToggle
              checked={filter.onlyChanged}
              onChange={(checked) =>
                setFilter((prev) => ({ ...prev, onlyChanged: checked }))
              }
              aria-label={t("PublishModpackModal.tree.filter.onlyChanged")}
            >
              {t("PublishModpackModal.tree.filter.onlyChanged")}
            </BlockToggle>
          </HStack>
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
      <Box maxH="xs" overflowY="auto">
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
            renderNode={({ node: { data } }) => (
              <PublishTreeRow
                instanceId={instanceId}
                data={data}
                context={context}
                draft={draft}
                view={view}
                label={label}
                category={marks.get(keyOf(data.path))}
                change={changeByKey.get(keyOf(data.path))}
                dirChanges={dirCounts.get(keyOf(data.path)) ?? 0}
                localDeleteCount={
                  localDeleteDirCounts.get(keyOf(data.path)) ?? 0
                }
                onDraftChange={onDraftChange}
              />
            )}
          />
        ) : (
          <Empty withIcon={false} size="sm" />
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
  label: (state: ShownState) => string;
  category: ChangeCategory | undefined;
  change: PublishChange | undefined;
  dirChanges: number;
  localDeleteCount: number;
  onDraftChange: (updater: (prev: PublishDraft) => PublishDraft) => void;
}

const PublishTreeRow: React.FC<PublishTreeRowProps> = ({
  instanceId,
  data,
  context,
  draft,
  view,
  label,
  category,
  change,
  dirChanges,
  localDeleteCount,
  onDraftChange,
}) => {
  const { t } = useTranslation();
  const [showDiff, setShowDiff] = useState(false);
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
            <Tag
              size="sm"
              variant="subtle"
              colorScheme={CHANGE_COLOR[category]}
              flexShrink={0}
            >
              <TagLabel>
                {t(`PublishModpackModal.tree.change.${category}`)}
              </TagLabel>
            </Tag>
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
            onClick={() => setShowDiff((prev) => !prev)}
          >
            {t(
              showDiff
                ? "PublishModpackModal.tree.diff.hide"
                : "PublishModpackModal.tree.diff.show"
            )}
          </Button>
        ) : null}
        {!isDir && !data.ghost && state === "include" ? (
          <Tooltip label={t("PublishModpackModal.tree.onceHint")}>
            <Box flexShrink={0}>
              <BlockToggle
                checked={view.isOnce(data.path)}
                onChange={(checked) =>
                  onDraftChange((prev) => setOnce(prev, data.path, checked))
                }
                aria-label={t("PublishModpackModal.tree.once")}
              >
                {t("PublishModpackModal.tree.once")}
              </BlockToggle>
            </Box>
          </Tooltip>
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
