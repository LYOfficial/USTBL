import type { TreeNode } from "@/components/common/tree-view";

/** One line of an expanded tree. */
export interface FlatRow<T = unknown> {
  node: TreeNode<T>;
  depth: number;
  isLeaf: boolean;
  isExpanded: boolean;
  /** 1-based place among the siblings, for assistive technology. */
  posInSet: number;
  setSize: number;
}

/**
 * The lines a tree shows: every node whose parents are all open, in display
 * order. Closed branches are not walked, so the cost follows what is shown.
 */
export const flattenTree = <T>(
  nodes: TreeNode<T>[],
  isExpanded: (id: string) => boolean
): FlatRow<T>[] => {
  const rows: FlatRow<T>[] = [];
  const visit = (list: TreeNode<T>[], depth: number) => {
    list.forEach((node, at) => {
      const children = node.children ?? [];
      const isLeaf = children.length === 0;
      const open = !isLeaf && isExpanded(node.id);
      rows.push({
        node,
        depth,
        isLeaf,
        isExpanded: open,
        posInSet: at + 1,
        setSize: list.length,
      });
      if (open) visit(children, depth + 1);
    });
  };
  visit(nodes, 0);
  return rows;
};

/**
 * The indent of a depth in rem. `indentSize` counts Chakra spacing units
 * (0.25rem each); a computed value rather than a theme token, so every depth
 * is one step deeper than the one above.
 */
export const indentRem = (depth: number, indentSize = 3): number =>
  (depth * indentSize) / 4;
