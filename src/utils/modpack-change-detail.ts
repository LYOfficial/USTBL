// Pure logic behind the local-changes dialog (plan 《整合包本地改动明细》§5):
// counts per class, the filter, and the directory tree the entries show in.
import {
  ModpackChangeClass,
  ModpackChangeDetailEntry,
} from "@/models/modpack-update";

/** Up to this many entries, the whole tree starts expanded. */
export const OPEN_LIMIT = 5;

/** The order of the class counts in the header. */
export const CLASS_ORDER: ModpackChangeClass[] = [
  "modified",
  "deleted",
  "toggled",
  "added",
  "onceChanged",
  "addedExcluded",
];

export const countByClass = (
  entries: ModpackChangeDetailEntry[]
): Record<ModpackChangeClass, number> => {
  const counts = Object.fromEntries(
    CLASS_ORDER.map((name) => [name, 0])
  ) as Record<ModpackChangeClass, number>;
  for (const entry of entries) counts[entry.class] += 1;
  return counts;
};

/** The number the status and the ledger show. */
export const countedTotal = (entries: ModpackChangeDetailEntry[]): number =>
  entries.filter((entry) => entry.counted).length;

export const filterEntries = (
  entries: ModpackChangeDetailEntry[],
  filter: ModpackChangeClass | null
): ModpackChangeDetailEntry[] =>
  filter ? entries.filter((entry) => entry.class === filter) : entries;

/** The tree shows every class but one; added files that will not publish get their own group. */
export const splitByGroup = (entries: ModpackChangeDetailEntry[]) => ({
  main: entries.filter((entry) => entry.class !== "addedExcluded"),
  excluded: entries.filter((entry) => entry.class === "addedExcluded"),
});

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
