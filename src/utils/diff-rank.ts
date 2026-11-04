// One order for every list of changed files: what can show a line diff comes
// first, then files replaced as a whole, then the rest. Each list says which
// rank a row has (`changeRank`, `conflictRank`, `entryRank`, `ignoredRank`);
// the sorting is shared.

/** 0 shows a line diff, 1 replaces the file as a whole, 2 is anything else. */
export type DiffRank = 0 | 1 | 2;

/**
 * A sorted copy: by rank, then by `compare` when given, then in the order the
 * list had. Ranks are computed once per item, not once per comparison.
 */
export function sortByRank<T>(
  items: readonly T[],
  rankOf: (item: T) => number,
  compare?: (a: T, b: T) => number
): T[] {
  return items
    .map((item, index) => ({ item, index, rank: rankOf(item) }))
    .sort(
      (a, b) =>
        a.rank - b.rank ||
        (compare ? compare(a.item, b.item) : 0) ||
        a.index - b.index
    )
    .map(({ item }) => item);
}
