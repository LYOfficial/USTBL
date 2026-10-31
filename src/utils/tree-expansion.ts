import type { TreeNode } from "@/components/common/tree-view";

export type ExpansionHistory = Map<string, boolean>;

const collectBranches = (
  nodes: TreeNode[],
  result = new Set<string>()
): Set<string> => {
  nodes.forEach((node) => {
    if (node.children?.length) {
      result.add(node.id);
      collectBranches(node.children, result);
    }
  });
  return result;
};

/** Remember branch state, including branches that are temporarily filtered out. */
export const syncExpandedState = (
  history: ExpansionHistory,
  _current: Set<string>,
  nodes: TreeNode[],
  defaultIds: Set<string>
): Set<string> => {
  const branches = collectBranches(nodes);
  branches.forEach((id) => {
    if (!history.has(id)) history.set(id, defaultIds.has(id));
  });
  return new Set(Array.from(branches).filter((id) => history.get(id) === true));
};

/** Apply one expansion request. The caller tracks request identity. */
export const applyExpansionRequest = (
  history: ExpansionHistory,
  ids: Set<string>
): void => {
  ids.forEach((id) => history.set(id, true));
};

export const branchIds = (nodes: TreeNode[]): Set<string> =>
  collectBranches(nodes);
