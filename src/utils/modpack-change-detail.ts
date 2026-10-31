// Pure logic behind the local-changes dialog (plan 《整合包本地改动明细》§11.2):
// what the body lists, the counts and filter over it, and the directory tree.
import {
  ModpackChangeClass,
  ModpackChangeDetailEntry,
} from "@/models/modpack-update";

/** Up to this many entries, the whole tree starts expanded. */
export const OPEN_LIMIT = 5;

/** The classes the dialog shows and counts, in header order. */
export const CLASS_ORDER: ModpackChangeClass[] = [
  "modified",
  "deleted",
  "toggled",
  "added",
];

const ALL_CLASSES: ModpackChangeClass[] = [
  ...CLASS_ORDER,
  "onceChanged",
  "addedExcluded",
];

const isAdded = (entry: ModpackChangeDetailEntry): boolean =>
  entry.class === "added" || entry.class === "addedExcluded";

/** A file the previous release's delete rule covers; the next update removes it. */
export const isRuleDeleted = (entry: ModpackChangeDetailEntry): boolean =>
  isAdded(entry) && entry.rule === "delete";

export const countByClass = (
  entries: ModpackChangeDetailEntry[]
): Record<ModpackChangeClass, number> => {
  const counts = Object.fromEntries(
    ALL_CLASSES.map((name) => [name, 0])
  ) as Record<ModpackChangeClass, number>;
  for (const entry of entries) counts[entry.class] += 1;
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
      CLASS_ORDER.includes(entry.class) &&
      !isRuleDeleted(entry)
  ),
  willDelete: entries.filter(isRuleDeleted),
});

export const filterEntries = (
  entries: ModpackChangeDetailEntry[],
  filter: ModpackChangeClass | null
): ModpackChangeDetailEntry[] =>
  filter ? entries.filter((entry) => entry.class === filter) : entries;
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
  const roots: ChangeTreeNode[] = [];
  const dirs = new Map<string, ChangeTreeNode>();
  for (const entry of entries) {
    const parts = entry.path.split("/").filter(Boolean);
    let siblings = roots;
    let current = "";
    parts.slice(0, -1).forEach((part) => {
      current = current ? `${current}/${part}` : part;
      const key = current.toLowerCase();
      let dir = dirs.get(key);
      if (!dir) {
        dir = {
          path: current,
          name: part,
          kind: "dir",
          count: 0,
          children: [],
        };
        dirs.set(key, dir);
        siblings.push(dir);
      }
      dir.count += 1;
      siblings = dir.children;
    });
    siblings.push({
      path: entry.path,
      name: parts[parts.length - 1] ?? entry.path,
      kind: "file",
      count: 1,
      entry,
      children: [],
    });
  }
  const sort = (nodes: ChangeTreeNode[]) => {
    nodes.sort((a, b) =>
      a.kind !== b.kind
        ? a.kind === "dir"
          ? -1
          : 1
        : a.name.localeCompare(b.name)
    );
    nodes.forEach((node) => sort(node.children));
  };
  sort(roots);
  return roots;
};

/** Few entries open the whole tree; more open the first level only. */
export const expandedDepth = (total: number): number =>
  total <= OPEN_LIMIT ? Infinity : 1;

/** The name the file has on disk, for "show in folder"; `null` when it is gone. */
export const revealPath = (entry: ModpackChangeDetailEntry): string | null => {
  if (entry.class === "deleted") return null;
  if (entry.class === "onceChanged" && entry.size === undefined) return null;
  return entry.disabled ? `${entry.path}.disabled` : entry.path;
};
