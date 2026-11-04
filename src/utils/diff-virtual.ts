import { ModpackDiffHunk } from "@/models/modpack-update";

/** Fixed height of one diff row, in px. */
export const DIFF_ROW_HEIGHT = 20;
/** Diffs with more rows than this scroll inside a fixed-height box. */
export const DIFF_VIRTUAL_THRESHOLD = 200;
/** Height of that box, in px. */
export const DIFF_VIEWPORT_HEIGHT = 400;
/** Rows rendered beyond each edge of the viewport. */
export const DIFF_OVERSCAN = 10;

/** Rows a set of hunks takes up: every line, plus one header per hunk. */
export const diffRowCount = (
  hunks: ModpackDiffHunk[],
  hideHunkHeaders: boolean
) => {
  let count = hideHunkHeaders ? 0 : hunks.length;
  for (const hunk of hunks) count += hunk.lines.length;
  return count;
};

export const shouldVirtualizeDiff = (rowCount: number) =>
  rowCount > DIFF_VIRTUAL_THRESHOLD;

/** The `[start, end)` rows to render when row `first` is at the top. */
export const diffVisibleRange = (first: number, rowCount: number) => {
  const visible = Math.ceil(DIFF_VIEWPORT_HEIGHT / DIFF_ROW_HEIGHT) + 1;
  return {
    start: Math.max(0, first - DIFF_OVERSCAN),
    end: Math.min(rowCount, first + visible + DIFF_OVERSCAN),
  };
};

/**
 * Width of a line in monospace columns, so the scrollable width can be set
 * without measuring. Errs wide: a tab counts as 8 and any non-Latin
 * character as 2.
 */
export const textColumns = (text: string) => {
  let columns = 0;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    columns += code === 9 ? 8 : code < 0x300 ? 1 : 2;
  }
  return columns;
};
