import { ModpackConflict, ModpackFileChange } from "@/models/modpack-update";

/**
 * The extensions the backend reads as text (`TEXT_EXTENSIONS` in
 * `helpers/modpack/text.rs`); a test keeps the two lists equal.
 */
export const TEXT_EXTENSIONS = [
  "cfg",
  "conf",
  "ini",
  "json",
  "lang",
  "properties",
  "toml",
  "txt",
];

/** The largest file the backend reads as text (`MAX_TEXT_SIZE`). */
export const MAX_TEXT_SIZE = 2 * 1024 * 1024;

export const hasTextExtension = (path: string) => {
  const name = path.split("/").pop() ?? path;
  const dot = name.lastIndexOf(".");
  return dot > 0 && TEXT_EXTENSIONS.includes(name.slice(dot + 1).toLowerCase());
};

/**
 * Whether an update or merge can be shown as a line diff, as far as the plan
 * tells: a text extension and both sides within the size limit. Encoding is
 * only known once the file is read, so the diff may still turn out unavailable.
 */
export const canShowChangeDiff = (change: ModpackFileChange) =>
  change.kind === "update" &&
  !change.restored &&
  hasTextExtension(change.path) &&
  (change.localSize ?? 0) <= MAX_TEXT_SIZE &&
  (change.remote?.size ?? 0) <= MAX_TEXT_SIZE;

/**
 * Where a conflict goes in the list: 0 has text blocks to read, 1 replaces a
 * file as a whole (a changed file without blocks, a file in the way, a file
 * managed again), 2 is the rest (a file the new version removes or restores).
 */
export const conflictRank = (conflict: ModpackConflict): 0 | 1 | 2 => {
  if (conflict.hunks.length > 0) return 0;
  switch (conflict.kind) {
    case "modifiedLocallyChangedRemotely":
    case "unmanagedFileInTheWay":
    case "remanage":
      return 1;
    default:
      return 2;
  }
};

/** Conflicts by rank; the order within a rank (the plan's) is kept. */
export const sortConflicts = (conflicts: ModpackConflict[]) =>
  conflicts
    .map((conflict, index) => ({ conflict, index }))
    .sort(
      (a, b) =>
        conflictRank(a.conflict) - conflictRank(b.conflict) || a.index - b.index
    )
    .map(({ conflict }) => conflict);

/** Changes with a diff to show first, each part by path. */
export const sortChangesByDiff = (changes: ModpackFileChange[]) =>
  [...changes].sort(
    (a, b) =>
      Number(canShowChangeDiff(b)) - Number(canShowChangeDiff(a)) ||
      a.path.localeCompare(b.path)
  );
