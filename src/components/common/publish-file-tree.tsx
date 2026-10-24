import {
  Box,
  Button,
  Center,
  HStack,
  IconButton,
  Switch,
  Tag,
  TagLabel,
  TagLeftIcon,
  Text,
  Tooltip,
} from "@chakra-ui/react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import {
  LuBan,
  LuCheck,
  LuMinus,
  LuTrash2,
  LuTriangleAlert,
  LuUndo2,
  LuX,
} from "react-icons/lu";
import { BeatLoader } from "react-spinners";
import Empty from "@/components/common/empty";
import TreeView, { TreeNode } from "@/components/common/tree-view";
import {
  PublishContext,
  PublishDraft,
  PublishNode,
  ShownState,
  buildPublishNodes,
  canRemoveFile,
  nextState,
  restoreInheritance,
  setDirState,
  setFileState,
  setOnce,
  viewOf,
} from "@/utils/modpack-publish-tree";

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
  context: PublishContext;
  draft: PublishDraft;
  isLoading: boolean;
  onDraftChange: (updater: (prev: PublishDraft) => PublishDraft) => void;
}

/**
 * Tri-state tree for choosing what a modpack version publishes: each node is
 * included, ignored or deleted. The state is shown as an icon and a label, and
 * the button that changes it works from the keyboard.
 */
const PublishFileTree: React.FC<PublishFileTreeProps> = ({
  context,
  draft,
  isLoading,
  onDraftChange,
}) => {
  const { t } = useTranslation();
  const nodes = useMemo(() => buildPublishNodes(context), [context]);
  const tree = useMemo(() => {
    const convert = (list: PublishNode[]): TreeNode<PublishNode>[] =>
      list.map((node) => ({
        id: node.path,
        data: node,
        children: convert(node.children),
      }));
    return convert(nodes);
  }, [nodes]);
  const view = useMemo(() => viewOf(context, draft), [context, draft]);

  const label = (state: ShownState) => t(`PublishModpackModal.tree.${state}`);

  return (
    <Box w="100%" maxH="xs" overflowY="auto">
      <Text fontSize="xs" className="secondary-text" mb={2}>
        {t("PublishModpackModal.tree.legend")}
      </Text>
      {isLoading ? (
        <Center py={4}>
          <BeatLoader size={10} color="gray" />
        </Center>
      ) : tree.length > 0 ? (
        <TreeView
          nodes={tree}
          renderNode={({ node: { data } }) => {
            const isDir = data.kind === "dir";
            const state = isDir
              ? view.dirState(data.path)
              : view.fileState(data.path);
            const canRemove =
              data.ghost && (isDir || canRemoveFile(draft, data.path));
            const next = nextState(state, { canRemove });
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

            return (
              <HStack spacing={2} fontSize="xs-sm" w="100%">
                <Button
                  size="xs"
                  variant={state === "include" ? "solid" : "outline"}
                  colorScheme={style.colorScheme}
                  leftIcon={style.icon}
                  minW="16"
                  flexShrink={0}
                  aria-label={description}
                  title={description}
                  onClick={apply}
                >
                  {label(state)}
                </Button>
                <Text
                  className="ellipsis-text"
                  opacity={data.ghost ? 0.7 : 1}
                  fontStyle={data.ghost ? "italic" : undefined}
                >
                  {data.name}
                </Text>
                {data.disabled ? (
                  <Tag size="sm" flexShrink={0}>
                    <TagLabel>
                      {t("PublishModpackModal.tree.disabled")}
                    </TagLabel>
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
                    <TagLabel>
                      {t("PublishModpackModal.tree.conflict")}
                    </TagLabel>
                  </Tag>
                ) : null}
                <Box flex="1" />
                {!isDir && !data.ghost && state === "include" ? (
                  <Tooltip label={t("PublishModpackModal.tree.onceHint")}>
                    <Box flexShrink={0}>
                      <Switch
                        size="sm"
                        isChecked={view.isOnce(data.path)}
                        onChange={(event) =>
                          onDraftChange((prev) =>
                            setOnce(prev, data.path, event.target.checked)
                          )
                        }
                      >
                        <Text fontSize="xs">
                          {t("PublishModpackModal.tree.once")}
                        </Text>
                      </Switch>
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
            );
          }}
        />
      ) : (
        <Empty withIcon={false} size="sm" />
      )}
    </Box>
  );
};

export default PublishFileTree;
