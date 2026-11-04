import { Box, HStack, IconButton } from "@chakra-ui/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { LuChevronDown, LuChevronRight } from "react-icons/lu";
import VirtualRows from "@/components/common/virtual-rows";
import {
  ExpansionHistory,
  applyBulkExpansion,
  applyExpansionRequest,
  branchIds,
  syncExpandedState,
} from "@/utils/tree-expansion";
import { flattenTree, indentRem } from "@/utils/tree-flatten";

export interface TreeNode<T = unknown> {
  id: string;
  data: T;
  children?: TreeNode<T>[];
}

export interface StructTreeNodeData {
  key: string;
  value: unknown;
}

export interface TreeNodeRenderContext<T = unknown> {
  node: TreeNode<T>;
  depth: number;
  isLeaf: boolean;
  isExpanded: boolean;
}

interface TreeViewProps<T = unknown> {
  nodes: TreeNode<T>[];
  renderNode: (context: TreeNodeRenderContext<T>) => React.ReactNode;
  /** Indent per level in Chakra spacing units (0.25rem each). */
  indentSize?: number;
  spacing?: number;
  /** The tallest the tree grows in px before it scrolls. */
  maxHeight?: number;
  defaultExpandedDepth?: number;
  preserveExpanded?: boolean;
  expandRequest?: { ids: Set<string> };
  /** Show every branch open without touching the remembered expansion. */
  expandAll?: boolean;
  /**
   * Opens or closes every branch of `nodes` each time a new command arrives
   * (compared by identity), whatever was open before.
   */
  command?: TreeCommand;
  leafPlaceholderWidth?: number;
}

export interface TreeCommand {
  type: "expand" | "collapse";
}

const collectExpandedIdsByDepth = <T,>(
  nodes: TreeNode<T>[],
  maxDepth: number,
  depth = 0,
  expandedIds = new Set<string>()
): Set<string> => {
  nodes.forEach((node) => {
    const children = node.children ?? [];
    if (children.length > 0 && depth < maxDepth) {
      expandedIds.add(node.id);
      collectExpandedIdsByDepth(children, maxDepth, depth + 1, expandedIds);
    }
  });
  return expandedIds;
};
export const buildStructTreeNodes = (
  data: unknown,
  parentId = "root"
): TreeNode<StructTreeNodeData>[] => {
  if (typeof data !== "object" || data === null) {
    return [];
  }

  return Object.entries(data).map(([key, value]) => {
    const id = `${parentId}.${key}`;
    return {
      id,
      data: { key, value },
      children:
        typeof value === "object" && value !== null
          ? buildStructTreeNodes(value, id)
          : [],
    };
  });
};

const TreeView = <T,>({
  nodes,
  renderNode,
  indentSize = 3,
  spacing = 1,
  maxHeight = 320,
  defaultExpandedDepth = 0,
  preserveExpanded = false,
  expandRequest,
  expandAll = false,
  command,
  leafPlaceholderWidth = 24,
}: TreeViewProps<T>) => {
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());
  // Branches the user closed while `expandAll` is on; dropped when it flips.
  const [closedIds, setClosedIds] = useState<Set<string>>(new Set());
  const expansionHistory = useRef<ExpansionHistory>(new Map());
  const lastExpandRequest = useRef<TreeViewProps<T>["expandRequest"]>();

  useEffect(() => {
    const defaults = collectExpandedIdsByDepth(nodes, defaultExpandedDepth);
    setExpandedIds((current) => {
      if (!preserveExpanded) return new Set([...defaults]);
      return syncExpandedState(
        expansionHistory.current,
        current,
        nodes,
        defaults
      );
    });
    if (
      preserveExpanded &&
      expandRequest &&
      lastExpandRequest.current !== expandRequest
    ) {
      applyExpansionRequest(expansionHistory.current, expandRequest.ids);
      lastExpandRequest.current = expandRequest;
      setExpandedIds((current) => new Set([...current, ...expandRequest.ids]));
    }
  }, [nodes, defaultExpandedDepth, preserveExpanded, expandRequest]);

  useEffect(() => {
    setClosedIds(new Set());
  }, [expandAll]);

  const lastCommand = useRef<TreeCommand>();
  useEffect(() => {
    if (!command || lastCommand.current === command) return;
    lastCommand.current = command;
    const branches = branchIds(nodes);
    const open = command.type === "expand";
    applyBulkExpansion(expansionHistory.current, branches, open);
    setExpandedIds(open ? branches : new Set());
    // With `expandAll` on, closed branches are what is tracked.
    setClosedIds(open ? new Set() : new Set(branches));
  }, [command, nodes]);

  const toggleExpanded = useCallback(
    (id: string) => {
      if (expandAll) {
        setClosedIds((prev) => {
          const next = new Set(prev);
          if (!next.delete(id)) next.add(id);
          return next;
        });
        return;
      }
      setExpandedIds((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else {
          next.add(id);
        }
        if (expansionHistory.current.has(id)) {
          expansionHistory.current.set(id, next.has(id));
        }
        return next;
      });
    },
    [expandAll]
  );

  const isOpen = useCallback(
    (id: string) => (expandAll ? !closedIds.has(id) : expandedIds.has(id)),
    [expandAll, closedIds, expandedIds]
  );
  // Only the lines that are shown exist; the list draws the ones in view.
  const rows = useMemo(() => flattenTree(nodes, isOpen), [nodes, isOpen]);

  const renderRow = (index: number) => {
    const { node, depth, isLeaf, isExpanded, posInSet, setSize } = rows[index];
    return (
      <HStack
        role="treeitem"
        aria-level={depth + 1}
        aria-posinset={posInSet}
        aria-setsize={setSize}
        aria-expanded={isLeaf ? undefined : isExpanded}
        spacing={1}
        w="100%"
        align="center"
        minH={6}
        pb={spacing}
        style={{ paddingLeft: `${indentRem(depth, indentSize)}rem` }}
      >
        {!isLeaf ? (
          <IconButton
            aria-label={isExpanded ? "collapse" : "expand"}
            icon={isExpanded ? <LuChevronDown /> : <LuChevronRight />}
            size="xs"
            variant="ghost"
            onClick={() => toggleExpanded(node.id)}
          />
        ) : leafPlaceholderWidth > 0 ? (
          <Box w={`${leafPlaceholderWidth}px`} flexShrink={0} />
        ) : null}
        <Box flex="1" minW={0}>
          {renderNode({ node, depth, isLeaf, isExpanded })}
        </Box>
      </HStack>
    );
  };

  return (
    <Box w="100%">
      <VirtualRows
        rowCount={rows.length}
        rowKey={(index) => rows[index].node.id}
        renderRow={renderRow}
        layoutKey={rows}
        maxHeight={maxHeight}
        role="tree"
        rowRole="presentation"
      />
    </Box>
  );
};
export default TreeView;
