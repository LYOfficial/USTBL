// Pure logic behind the local-changes dialog (plan 《整合包本地改动明细》§11.2):
// what the body lists, the counts and filter over it, and the directory tree.
import {
  ModpackChangeClass,
  ModpackChangeDetailEntry,
  ModpackIgnoredItem,
} from "@/models/modpack-update";
import { DiffRank, sortByRank } from "@/utils/diff-rank";
import {
  compareDirsFirst,
  compareNames,
  createPathTree,
  placePath,
  sortPathTree,
} from "@/utils/path-tree";

/** Up to this many entries, the whole tree starts expanded. */
export const OPEN_LIMIT = 5;

export type ChangeFilter =
  | Exclude<ModpackChangeClass, "toggled">
  | "enable"
  | "disable";

/** The classes the dialog shows and counts, in header order. */
export const CLASS_ORDER: ChangeFilter[] = [
  "modified",
  "deleted",
  "enable",
  "disable",
  "added",
];

const ALL_CLASSES: ChangeFilter[] = [
  ...CLASS_ORDER,
  "onceChanged",
  "addedExcluded",
];

export const displayClass = (entry: ModpackChangeDetailEntry): ChangeFilter =>
  entry.class === "toggled"
    ? entry.disabled
      ? "disable"
      : "enable"
    : entry.class;

const isAdded = (entry: ModpackChangeDetailEntry): boolean =>
  entry.class === "added" || entry.class === "addedExcluded";

/** A file the previous release's delete rule covers; the next update removes it. */
export const isRuleDeleted = (entry: ModpackChangeDetailEntry): boolean =>
  isAdded(entry) && entry.rule === "delete";

export const countByClass = (
  entries: ModpackChangeDetailEntry[]
): Record<ChangeFilter, number> => {
  const counts = Object.fromEntries(
    ALL_CLASSES.map((name) => [name, 0])
  ) as Record<ChangeFilter, number>;
  for (const entry of entries) counts[displayClass(entry)] += 1;
  return counts;
};

/**
 * The body lists the counted entries; files a delete rule covers get their own
 * group; first-install changes and uncounted additions are not shown, since an
 * update leaves them alone.
 */
export const splitByGroup = (entries: ModpackChangeDetailEntry[]) => ({
  main: entries.filter(
    (entry) =>
      entry.counted &&
      CLASS_ORDER.includes(displayClass(entry)) &&
      !isRuleDeleted(entry)
  ),
  willDelete: entries.filter(isRuleDeleted),
});

export const filterEntries = (
  entries: ModpackChangeDetailEntry[],
  filter: ChangeFilter | null
): ModpackChangeDetailEntry[] =>
  filter
    ? entries.filter((entry) =>
        filter === "enable" || filter === "disable"
          ? entry.class === "toggled" &&
            (filter === "disable" ? entry.disabled : !entry.disabled)
          : entry.class === filter
      )
    : entries;
export interface ChangeTreeNode {
  /** The path as the first entry under it spells it. */
  path: string;
  name: string;
  kind: "file" | "dir";
  /** Entries at or below this node. */
  count: number;
  entry?: ModpackChangeDetailEntry;
  children: ChangeTreeNode[];
}

/** Directories first, then files, each by name; directories merge case-insensitively. */
export const buildChangeTree = (
  entries: ModpackChangeDetailEntry[]
): ChangeTreeNode[] => {
  const tree = createPathTree<ChangeTreeNode>();
  for (const entry of entries) {
    placePath(tree, entry.path, {
      makeDir: (path, name) => ({
        path,
        name,
        kind: "dir" as const,
        count: 0,
        children: [],
      }),
      makeLeaf: (path, name) => ({
        path: entry.path,
        name,
        kind: "file" as const,
        count: 1,
        entry,
        children: [],
      }),
      onDir: (dir) => {
        dir.count += 1;
      },
      mergeLeaves: false,
    });
  }
  return sortPathTree(tree.roots, (a, b) =>
    a.kind === "file" && b.kind === "file" && a.entry && b.entry
      ? entryRank(a.entry) - entryRank(b.entry) || compareNames(a.name, b.name)
      : compareDirsFirst(a, b)
  );
};

/**
 * Where an entry goes among its siblings: 0 shows a line diff, 1 is a changed
 * file with no diff to show (not text, no copy of the original), 2 is a file
 * that is added, deleted, switched or first-install only.
 */
export const entryRank = (entry: ModpackChangeDetailEntry): DiffRank => {
  if (entry.class !== "modified") return 2;
  return entry.hunks && entry.hunks.length > 0 ? 0 : 1;
};

/** Ignored items: spots ignored show a diff, a whole file does not, an added file is the rest. */
export const ignoredRank = (item: ModpackIgnoredItem): DiffRank =>
  item.added ? 2 : item.whole ? 1 : 0;

export const sortEntriesByDiff = (entries: ModpackChangeDetailEntry[]) =>
  sortByRank(entries, entryRank);

export const sortIgnoredByDiff = (items: ModpackIgnoredItem[]) =>
  sortByRank(items, ignoredRank);

/** Few entries open the whole tree; more open the first level only. */
export const expandedDepth = (total: number): number =>
  total <= OPEN_LIMIT ? Infinity : 1;

/** The name the file has on disk, for "show in folder"; `null` when it is gone. */
export const revealPath = (entry: ModpackChangeDetailEntry): string | null => {
  if (entry.class === "deleted") return null;
  if (entry.class === "onceChanged" && entry.size === undefined) return null;
  return entry.disabled ? `${entry.path}.disabled` : entry.path;
};
