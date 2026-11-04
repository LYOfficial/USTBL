import {
  Box,
  Collapse,
  HStack,
  Icon,
  Text,
  useColorModeValue,
  useDisclosure,
} from "@chakra-ui/react";
import React, { useEffect, useMemo, useRef, useState } from "react";
import { LuChevronDown, LuChevronRight } from "react-icons/lu";
import {
  ModpackDiffEdit,
  ModpackDiffHunk,
  ModpackDiffLine,
  ModpackDiffLineKind,
} from "@/models/modpack-update";
import {
  DIFF_ROW_HEIGHT,
  DIFF_VIEWPORT_HEIGHT,
  diffRowCount,
  diffVisibleRange,
  shouldVirtualizeDiff,
  textColumns,
} from "@/utils/diff-virtual";

const MONO =
  "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, 'Liberation Mono', monospace";

/** Colors of the GitHub diff view, light and dark. */
const useDiffColors = () => {
  const border = useColorModeValue("#d0d7de", "#30363d");
  const headerBg = useColorModeValue("#f6f8fa", "#161b22");
  const hunkBg = useColorModeValue("#ddf4ff", "rgba(56,139,253,0.1)");
  const hunkGutter = useColorModeValue("#b6e3ff", "rgba(56,139,253,0.2)");
  const hunkText = useColorModeValue("#57606a", "#8b949e");
  const addBg = useColorModeValue("#e6ffec", "rgba(46,160,67,0.15)");
  const addGutter = useColorModeValue("#ccffd8", "rgba(63,185,80,0.3)");
  const deleteBg = useColorModeValue("#ffebe9", "rgba(248,81,73,0.1)");
  const deleteGutter = useColorModeValue("#ffd7d5", "rgba(248,81,73,0.3)");
  const number = useColorModeValue("#6e7781", "#6e7681");
  const addStat = useColorModeValue("#1a7f37", "#3fb950");
  const deleteStat = useColorModeValue("#cf222e", "#f85149");
  return useMemo(
    () => ({
      border,
      headerBg,
      hunkBg,
      hunkGutter,
      hunkText,
      addBg,
      addGutter,
      deleteBg,
      deleteGutter,
      number,
      addStat,
      deleteStat,
    }),
    [
      border,
      headerBg,
      hunkBg,
      hunkGutter,
      hunkText,
      addBg,
      addGutter,
      deleteBg,
      deleteGutter,
      number,
      addStat,
      deleteStat,
    ]
  );
};

type DiffColors = ReturnType<typeof useDiffColors>;

const MARKS: Record<ModpackDiffLineKind, string> = {
  context: " ",
  add: "+",
  delete: "-",
};

/** Added and deleted line counts over a set of hunks. */
export const diffStats = (hunks: ModpackDiffHunk[]) => {
  let additions = 0;
  let deletions = 0;
  for (const hunk of hunks) {
    for (const line of hunk.lines) {
      if (line.kind === "add") additions += 1;
      if (line.kind === "delete") deletions += 1;
    }
  }
  return { additions, deletions };
};

/**
 * A hunk that shows `before` turning into `after`, numbered from `start`
 * (1-based) on both sides. Lines the two share at either end are context.
 */
export const regionHunk = (
  start: number,
  before: string[],
  after: string[]
): ModpackDiffHunk => {
  let prefix = 0;
  while (
    prefix < before.length &&
    prefix < after.length &&
    before[prefix] === after[prefix]
  ) {
    prefix += 1;
  }
  let suffix = 0;
  while (
    suffix < before.length - prefix &&
    suffix < after.length - prefix &&
    before[before.length - 1 - suffix] === after[after.length - 1 - suffix]
  ) {
    suffix += 1;
  }
  const lines: ModpackDiffLine[] = [];
  const context = (index: number, offset: number) =>
    lines.push({
      kind: "context",
      oldNumber: start + index,
      newNumber: start + index + offset,
      text: before[index],
    });
  for (let i = 0; i < prefix; i++) context(i, 0);
  for (let i = prefix; i < before.length - suffix; i++) {
    lines.push({ kind: "delete", oldNumber: start + i, text: before[i] });
  }
  for (let i = prefix; i < after.length - suffix; i++) {
    lines.push({ kind: "add", newNumber: start + i, text: after[i] });
  }
  const shift = after.length - before.length;
  for (let i = before.length - suffix; i < before.length; i++) {
    context(i, shift);
  }
  return {
    oldStart: start,
    oldLines: before.length,
    newStart: start,
    newLines: after.length,
    lines,
  };
};

const CELL_PADDING = "0 0.5rem";

/**
 * Inline styles of the diff rows, computed once per color mode so that
 * thousands of rows don't each go through the style system.
 */
const buildRowStyles = (
  colors: DiffColors,
  gutterWidth: string,
  virtual: boolean
) => {
  const gutter = (bg?: string): React.CSSProperties => ({
    flex: "0 0 auto",
    minWidth: gutterWidth,
    padding: CELL_PADDING,
    textAlign: "right",
    color: colors.number,
    background: bg,
    userSelect: "none",
  });
  const row = (bg?: string): React.CSSProperties => ({
    display: "flex",
    minWidth: "fit-content",
    background: bg,
    ...(virtual && {
      position: "absolute",
      left: 0,
      width: "100%",
      height: DIFF_ROW_HEIGHT,
    }),
  });
  return {
    row: {
      context: row(),
      add: row(colors.addBg),
      delete: row(colors.deleteBg),
    } as Record<ModpackDiffLineKind, React.CSSProperties>,
    gutter: {
      context: gutter(),
      add: gutter(colors.addGutter),
      delete: gutter(colors.deleteGutter),
    } as Record<ModpackDiffLineKind, React.CSSProperties>,
    mark: {
      flex: "0 0 auto",
      width: "1.5em",
      textAlign: "center",
      userSelect: "none",
    } as React.CSSProperties,
    text: { whiteSpace: "pre", paddingRight: "1rem" } as React.CSSProperties,
    action: {
      marginLeft: "auto",
      padding: CELL_PADDING,
      position: "sticky",
      right: 0,
      flex: "0 0 auto",
      ...(virtual && { display: "flex", alignItems: "center", zIndex: 1 }),
    } as React.CSSProperties,
    header: row(colors.hunkBg),
    headerGutter: {
      flex: "0 0 auto",
      minWidth: `calc(${gutterWidth} * 2 + 2rem + 1.5em)`,
      background: colors.hunkGutter,
    } as React.CSSProperties,
    headerText: {
      whiteSpace: "pre",
      padding: CELL_PADDING,
      color: colors.hunkText,
    } as React.CSSProperties,
  };
};

type RowStyles = ReturnType<typeof buildRowStyles>;

const DiffRow = React.memo(function DiffRow({
  line,
  styles,
  top,
  action,
}: {
  line: ModpackDiffLine;
  styles: RowStyles;
  top?: number;
  action?: React.ReactNode;
}) {
  const rowStyle =
    top === undefined
      ? styles.row[line.kind]
      : { ...styles.row[line.kind], top };
  return (
    <div style={rowStyle}>
      <span style={styles.gutter[line.kind]}>{line.oldNumber ?? ""}</span>
      <span style={styles.gutter[line.kind]}>{line.newNumber ?? ""}</span>
      <span style={styles.mark}>{MARKS[line.kind]}</span>
      <span style={styles.text}>{line.text === "" ? " " : line.text}</span>
      {action && <span style={styles.action}>{action}</span>}
    </div>
  );
});

const HunkHeaderRow: React.FC<{
  hunk: ModpackDiffHunk;
  styles: RowStyles;
  top?: number;
}> = ({ hunk, styles, top }) => (
  <div style={top === undefined ? styles.header : { ...styles.header, top }}>
    <span style={styles.headerGutter} />
    <span style={styles.headerText}>
      {`@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`}
    </span>
  </div>
);

interface DiffHunksProps {
  hunks: ModpackDiffHunk[];
  /** Hide the `@@ … @@` rows, for a single region shown on its own. */
  hideHunkHeaders?: boolean;
  /**
   * A control shown on the first line of each change the hunk lists in
   * `edits` (e.g. "ignore this spot").
   */
  renderEditAction?: (
    edit: ModpackDiffEdit,
    hunk: ModpackDiffHunk
  ) => React.ReactNode;
}

/** The edit control for line `index` of `hunk`, if it starts a change. */
const lineAction = (
  hunk: ModpackDiffHunk,
  index: number,
  renderEditAction: DiffHunksProps["renderEditAction"]
) => {
  const line = hunk.lines[index];
  const edit =
    line.edit !== undefined && line.edit !== null
      ? hunk.edits?.[line.edit]
      : undefined;
  const first = edit !== undefined && hunk.lines[index - 1]?.edit !== line.edit;
  return first && renderEditAction?.(edit, hunk);
};

/** A diff row of a long diff: a hunk header (`index` -1) or a line. */
interface FlatRow {
  hunk: ModpackDiffHunk;
  index: number;
}

/**
 * A long diff in a fixed-height box that renders only the rows in view.
 * The inner element is as wide as the longest line (in `ch`, the monospace
 * advance) and rows are absolutely placed at full inner width, so one scroll
 * container scrolls everything sideways and row backgrounds span all of it.
 */
const VirtualDiffHunks: React.FC<
  DiffHunksProps & { colors: DiffColors; rowCount: number }
> = ({ hunks, hideHunkHeaders, renderEditAction, colors, rowCount }) => {
  const [first, setFirst] = useState(0);
  const { rows, gutterWidth, contentWidth } = useMemo(() => {
    const rows: FlatRow[] = [];
    let columns = 0;
    let maxNumber = 0;
    for (const hunk of hunks) {
      if (!hideHunkHeaders) {
        rows.push({ hunk, index: -1 });
        columns = Math.max(columns, 40);
      }
      hunk.lines.forEach((line, index) => {
        rows.push({ hunk, index });
        columns = Math.max(columns, textColumns(line.text));
        maxNumber = Math.max(
          maxNumber,
          line.oldNumber ?? 0,
          line.newNumber ?? 0
        );
      });
    }
    const gutterWidth = `max(3.25em, calc(${String(maxNumber).length}ch + 1rem))`;
    return {
      rows,
      gutterWidth,
      contentWidth: `calc(${gutterWidth} * 2 + 1.5em + ${columns}ch + 1rem)`,
    };
  }, [hunks, hideHunkHeaders]);
  const styles = useMemo(
    () => buildRowStyles(colors, gutterWidth, true),
    [colors, gutterWidth]
  );
  const { start, end } = diffVisibleRange(first, rowCount);
  return (
    <Box
      fontFamily={MONO}
      fontSize="xs"
      lineHeight={`${DIFF_ROW_HEIGHT}px`}
      h={`${DIFF_VIEWPORT_HEIGHT}px`}
      overflow="auto"
      role="table"
      tabIndex={0}
      onScroll={(e) =>
        setFirst(Math.floor(e.currentTarget.scrollTop / DIFF_ROW_HEIGHT))
      }
    >
      <div
        style={{
          position: "relative",
          minWidth: "100%",
          width: contentWidth,
          height: rowCount * DIFF_ROW_HEIGHT,
        }}
      >
        {rows.slice(start, end).map(({ hunk, index }, offset) => {
          const top = (start + offset) * DIFF_ROW_HEIGHT;
          return index < 0 ? (
            <HunkHeaderRow
              key={start + offset}
              hunk={hunk}
              styles={styles}
              top={top}
            />
          ) : (
            <DiffRow
              key={start + offset}
              line={hunk.lines[index]}
              styles={styles}
              top={top}
              action={lineAction(hunk, index, renderEditAction)}
            />
          );
        })}
      </div>
    </Box>
  );
};

/**
 * The body of a GitHub-style unified diff. A diff of more than
 * `DIFF_VIRTUAL_THRESHOLD` rows scrolls inside a box and renders only the
 * rows in view; a shorter one is laid out in full.
 */
export const DiffHunks = React.memo(function DiffHunks({
  hunks,
  hideHunkHeaders = false,
  renderEditAction,
}: DiffHunksProps) {
  const colors = useDiffColors();
  const rowCount = diffRowCount(hunks, hideHunkHeaders);
  const styles = useMemo(
    () => buildRowStyles(colors, "3.25em", false),
    [colors]
  );
  if (shouldVirtualizeDiff(rowCount)) {
    return (
      <VirtualDiffHunks
        hunks={hunks}
        hideHunkHeaders={hideHunkHeaders}
        renderEditAction={renderEditAction}
        colors={colors}
        rowCount={rowCount}
      />
    );
  }
  return (
    <Box
      fontFamily={MONO}
      fontSize="xs"
      lineHeight={`${DIFF_ROW_HEIGHT}px`}
      overflowX="auto"
      role="table"
    >
      {hunks.map((hunk, index) => (
        <React.Fragment key={index}>
          {!hideHunkHeaders && <HunkHeaderRow hunk={hunk} styles={styles} />}
          {hunk.lines.map((line, lineIndex) => (
            <DiffRow
              key={lineIndex}
              line={line}
              styles={styles}
              action={lineAction(hunk, lineIndex, renderEditAction)}
            />
          ))}
        </React.Fragment>
      ))}
    </Box>
  );
});

interface DiffFileProps {
  path: string;
  /** Shown as `+a −d` next to the path. */
  hunks?: ModpackDiffHunk[] | null;
  /** Badges or notes after the path. */
  tags?: React.ReactNode;
  /** Buttons at the right end of the header. */
  actions?: React.ReactNode;
  defaultOpen?: boolean;
  /**
   * Set to let the parent hold the state, as a card in a virtualized list must:
   * the card is destroyed when it scrolls out of view.
   */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /**
   * Opens or closes the file whenever `token` changes (and decides the first
   * state when set), while the header still toggles it by hand in between.
   */
  expandSignal?: DiffExpandSignal;
  children?: React.ReactNode;
}

export interface DiffExpandSignal {
  open: boolean;
  token: number;
}

/** A file in a GitHub-style diff: a header bar over a collapsible body. */
export const DiffFile: React.FC<DiffFileProps> = ({
  path,
  hunks,
  tags,
  actions,
  defaultOpen = true,
  open,
  onOpenChange,
  expandSignal,
  children,
}) => {
  const colors = useDiffColors();
  const { isOpen, onToggle, onOpen, onClose } = useDisclosure({
    defaultIsOpen: expandSignal ? expandSignal.open : defaultOpen,
    isOpen: open,
    onOpen: () => onOpenChange?.(true),
    onClose: () => onOpenChange?.(false),
  });
  const handledToken = useRef(expandSignal?.token);
  useEffect(() => {
    if (!expandSignal || expandSignal.token === handledToken.current) return;
    handledToken.current = expandSignal.token;
    if (expandSignal.open) onOpen();
    else onClose();
  }, [expandSignal, onOpen, onClose]);
  const stats = hunks ? diffStats(hunks) : null;
  return (
    <Box
      borderWidth="1px"
      borderColor={colors.border}
      borderRadius="md"
      overflow="hidden"
    >
      <HStack
        bg={colors.headerBg}
        px={2}
        py={1.5}
        spacing={2}
        borderBottomWidth={isOpen && children ? "1px" : 0}
        borderColor={colors.border}
      >
        <HStack
          as="button"
          type="button"
          spacing={1.5}
          flex={1}
          minW={0}
          onClick={onToggle}
          aria-expanded={isOpen}
          textAlign="left"
        >
          <Icon as={isOpen ? LuChevronDown : LuChevronRight} flexShrink={0} />
          <Text
            fontFamily={MONO}
            fontSize="xs"
            fontWeight="semibold"
            noOfLines={1}
            wordBreak="break-all"
          >
            {path}
          </Text>
          {stats && (stats.additions > 0 || stats.deletions > 0) && (
            <Text fontFamily={MONO} fontSize="xs" flexShrink={0}>
              <Box as="span" color={colors.addStat}>
                +{stats.additions}
              </Box>{" "}
              <Box as="span" color={colors.deleteStat}>
                −{stats.deletions}
              </Box>
            </Text>
          )}
          {tags}
        </HStack>
        {actions && (
          <HStack spacing={1} flexShrink={0}>
            {actions}
          </HStack>
        )}
      </HStack>
      {children && (
        <Collapse in={isOpen} animateOpacity={false} unmountOnExit>
          {children}
        </Collapse>
      )}
    </Box>
  );
};
