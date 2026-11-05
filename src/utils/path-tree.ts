// Turns a list of slash-separated paths into a nested tree. The publish tree,
// the local-changes dialog and the export dialog each decide what a node holds;
// the walk down the path, the merging of directories and the order are shared.

export interface PathTreeNode<N> {
  children: N[];
}

export interface PathTree<N extends PathTreeNode<N>> {
  roots: N[];
  byKey: Map<string, N>;
}

export const createPathTree = <N extends PathTreeNode<N>>(): PathTree<N> => ({
  roots: [],
  byKey: new Map(),
});

export interface PlaceOptions<N> {
  /** What two segments must share to be one node; the lowercased path by default. */
  keyOf?: (path: string) => string;
  makeDir: (path: string, name: string) => N;
  makeLeaf: (path: string, name: string) => N;
  /** Called for every directory the path runs through, new or not. */
  onDir?: (dir: N) => void;
  /** Called for the last segment; `created` is false when a node was there already. */
  onLeaf?: (node: N, created: boolean) => void;
  /** Whether two paths with the same key are one node. On by default. */
  mergeLeaves?: boolean;
}

const lower = (path: string): string => path.toLowerCase();

/** Put one path into the tree, creating the directories above it. */
export const placePath = <N extends PathTreeNode<N>>(
  tree: PathTree<N>,
  path: string,
  options: PlaceOptions<N>
): void => {
  const keyOf = options.keyOf ?? lower;
  const mergeLeaves = options.mergeLeaves ?? true;
  const parts = path.split("/").filter(Boolean);
  let siblings = tree.roots;
  let current = "";
  parts.forEach((part, at) => {
    current = current ? `${current}/${part}` : part;
    const last = at === parts.length - 1;
    const key = keyOf(current);
    let node = last && !mergeLeaves ? undefined : tree.byKey.get(key);
    const created = !node;
    if (!node) {
      node = last
        ? options.makeLeaf(current, part)
        : options.makeDir(current, part);
      if (!last || mergeLeaves) tree.byKey.set(key, node);
      siblings.push(node);
    }
    if (last) options.onLeaf?.(node, created);
    else options.onDir?.(node);
    siblings = node.children;
  });
};

const collator = new Intl.Collator();

/** Order by name in the user's locale; one collator, since `localeCompare` builds a new one per call. */
export const compareNames = (a: string, b: string): number =>
  collator.compare(a, b);

/** Directories first, then files, each by name. */
export const compareDirsFirst = (
  a: { kind: "file" | "dir"; name: string },
  b: { kind: "file" | "dir"; name: string }
): number =>
  a.kind !== b.kind
    ? a.kind === "dir"
      ? -1
      : 1
    : compareNames(a.name, b.name);

/** Sort every level in place. */
export const sortPathTree = <N extends PathTreeNode<N>>(
  nodes: N[],
  compare: (a: N, b: N) => number
): N[] => {
  nodes.sort(compare);
  nodes.forEach((node) => sortPathTree(node.children, compare));
  return nodes;
};

export interface IdTreeNode<N> {
  id: string;
  data: N;
  children: IdTreeNode<N>[];
}

/** Wrap path nodes for `TreeView`, which needs an id on every node. */
export const toTreeNodes = <N extends PathTreeNode<N>>(
  nodes: N[],
  idOf: (node: N) => string
): IdTreeNode<N>[] =>
  nodes.map((node) => ({
    id: idOf(node),
    data: node,
    children: toTreeNodes(node.children, idOf),
  }));
