import { Box, Center, Checkbox, HStack, Text } from "@chakra-ui/react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { BeatLoader } from "react-spinners";
import Empty from "@/components/common/empty";
import TreeView, { TreeNode } from "@/components/common/tree-view";
import { useLauncherConfig } from "@/contexts/config";

interface FileTreeData {
  name: string;
  path: string;
  isFile: boolean;
  leafIds: string[];
}

export const buildFileTree = (paths: string[]): TreeNode<FileTreeData>[] => {
  type InternalNode = {
    name: string;
    path: string;
    isFile: boolean;
    children: Map<string, InternalNode>;
  };

  const root = new Map<string, InternalNode>();
  for (const rawPath of paths) {
    const parts = rawPath.split("/").filter(Boolean);
    let current = root;
    let currentPath = "";
    parts.forEach((part, index) => {
      currentPath = currentPath ? `${currentPath}/${part}` : part;
      let node = current.get(part);
      if (!node) {
        node = {
          name: part,
          path: currentPath,
          isFile: false,
          children: new Map<string, InternalNode>(),
        };
        current.set(part, node);
      }
      if (index === parts.length - 1) {
        node.isFile = true;
      }
      current = node.children;
    });
  }

  const convert = (map: Map<string, InternalNode>): TreeNode<FileTreeData>[] =>
    Array.from(map.values())
      .sort((a, b) => {
        if (a.isFile !== b.isFile) return a.isFile ? 1 : -1;
        return a.name.localeCompare(b.name);
      })
      .map((node) => {
        const children = convert(node.children);
        const leafIds =
          children.length === 0
            ? [node.path]
            : children.flatMap((child) => child.data.leafIds);

        return {
          id: node.path,
          data: {
            name: node.name,
            path: node.path,
            isFile: node.isFile,
            leafIds,
          },
          children,
        };
      });

  return convert(root);
};

const ROOT_FILE_CATEGORY_LOCALE_KEYS: Partial<Record<string, string>> = {
  mods: "mods",
  config: "config",
  shaderpacks: "shaderpacks",
  resourcepacks: "resourcepacks",
  "options.txt": "optionsTxt",
  saves: "saves",
};

interface ModpackFileTreeProps {
  paths: string[];
  isLoading: boolean;
  selectedFiles: Set<string>;
  onSelectedFilesChange: (updater: (prev: Set<string>) => Set<string>) => void;
}

/**
 * Checkbox tree used to pick which instance files go into a modpack.
 */
const ModpackFileTree: React.FC<ModpackFileTreeProps> = ({
  paths,
  isLoading,
  selectedFiles,
  onSelectedFilesChange,
}) => {
  const { t, i18n } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const fileTree = useMemo(() => buildFileTree(paths), [paths]);

  return (
    <Box w="100%" maxH="xs" overflowY="auto">
      {isLoading ? (
        <Center py={4}>
          <BeatLoader size={10} color="gray" />
        </Center>
      ) : fileTree.length > 0 ? (
        <TreeView
          nodes={fileTree}
          renderNode={({ node, depth }) => {
            const selectedCount = node.data.leafIds.filter((path) =>
              selectedFiles.has(path)
            ).length;
            const isChecked =
              node.data.leafIds.length > 0 &&
              selectedCount === node.data.leafIds.length;
            const isIndeterminate =
              selectedCount > 0 && selectedCount < node.data.leafIds.length;
            const translationKey =
              depth === 0
                ? ROOT_FILE_CATEGORY_LOCALE_KEYS[node.data.name]
                : undefined;
            const showTranslation =
              translationKey && !i18n.resolvedLanguage?.startsWith("en");

            return (
              <Checkbox
                isChecked={isChecked}
                isIndeterminate={isIndeterminate}
                onChange={(e) => {
                  const checked = e.target.checked;
                  onSelectedFilesChange((prev) => {
                    const next = new Set(prev);
                    node.data.leafIds.forEach((path) => {
                      if (checked) {
                        next.add(path);
                      } else {
                        next.delete(path);
                      }
                    });
                    return next;
                  });
                }}
                colorScheme={primaryColor}
                size="sm"
              >
                <HStack spacing={2} fontSize="xs-sm">
                  <Text className="ellipsis-text">{node.data.name}</Text>
                  {showTranslation ? (
                    <Text className="secondary-text">
                      {t(
                        `ExportModpackModal.fileTreeTranslation.${translationKey}`
                      )}
                    </Text>
                  ) : null}
                </HStack>
              </Checkbox>
            );
          }}
        />
      ) : (
        <Empty withIcon={false} size="sm" />
      )}
    </Box>
  );
};

export default ModpackFileTree;
