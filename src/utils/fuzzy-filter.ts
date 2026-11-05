// Fuzzy file-path filter shared by the publish tree, the update preview and the
// local-changes dialog. A query is split on whitespace; a path matches when
// every term occurs in it as a subsequence, ignoring case and the kind of slash.

/** More matching files than this and the tree keeps the user's own expansion. Only the rows in view are drawn, so this guards the cost of opening many branches, not of rendering them. */
export const AUTO_EXPAND_LIMIT = 5000;

const normalize = (text: string): string =>
  text.toLowerCase().replace(/\\/g, "/");

/** The lowercase terms of a query; empty when the query has none. */
export const queryTerms = (query: string): string[] =>
  normalize(query).split(/\s+/).filter(Boolean);

const hasSubsequence = (haystack: string, needle: string): boolean => {
  let at = 0;
  for (let i = 0; i < haystack.length && at < needle.length; i += 1) {
    if (haystack.charCodeAt(i) === needle.charCodeAt(at)) at += 1;
  }
  return at === needle.length;
};

export const isQueryActive = (query: string): boolean =>
  queryTerms(query).length > 0;

/** Parse the query once; the returned test is cheap enough for every file. */
export const createPathMatcher = (
  query: string
): ((path: string) => boolean) => {
  const terms = queryTerms(query);
  if (terms.length === 0) return () => true;
  return (path) => {
    const normalized = normalize(path);
    return terms.every((term) => hasSubsequence(normalized, term));
  };
};

export const matchesQuery = (path: string, query: string): boolean =>
  createPathMatcher(query)(path);

export interface FilterableNode {
  path: string;
  kind: "file" | "dir";
  children: FilterableNode[];
}

/**
 * The tree with only the files that match, and the directories above them.
 * The input itself is returned for an empty query; a directory is never
 * matched on its own, so one with no matching file below it drops out.
 */
export const filterTreeByQuery = <T extends FilterableNode>(
  nodes: T[],
  query: string
): T[] => {
  if (!isQueryActive(query)) return nodes;
  const matches = createPathMatcher(query);
  const keep = (list: T[]): T[] =>
    list.flatMap((node) => {
      if (node.kind === "file") return matches(node.path) ? [node] : [];
      const children = keep(node.children as T[]);
      return children.length > 0 ? [{ ...node, children }] : [];
    });
  return keep(nodes);
};

/** How many files a tree holds. */
export const countFiles = (nodes: FilterableNode[]): number =>
  nodes.reduce(
    (sum, node) => sum + (node.kind === "file" ? 1 : countFiles(node.children)),
    0
  );
