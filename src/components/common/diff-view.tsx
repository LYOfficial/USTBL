import {
  Box,
  Collapse,
  Flex,
  HStack,
  Icon,
  Text,
  useColorModeValue,
  useDisclosure,
} from "@chakra-ui/react";
import React from "react";
import { LuChevronDown, LuChevronRight } from "react-icons/lu";
import {
  ModpackDiffEdit,
  ModpackDiffHunk,
  ModpackDiffLine,
  ModpackDiffLineKind,
} from "@/models/modpack-update";

const MONO =
  "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, 'Liberation Mono', monospace";

/** Colors of the GitHub diff view, light and dark. */
const useDiffColors = () => ({
  border: useColorModeValue("#d0d7de", "#30363d"),
  headerBg: useColorModeValue("#f6f8fa", "#161b22"),
  hunkBg: useColorModeValue("#ddf4ff", "rgba(56,139,253,0.1)"),
  hunkGutter: useColorModeValue("#b6e3ff", "rgba(56,139,253,0.2)"),
  hunkText: useColorModeValue("#57606a", "#8b949e"),
  addBg: useColorModeValue("#e6ffec", "rgba(46,160,67,0.15)"),
  addGutter: useColorModeValue("#ccffd8", "rgba(63,185,80,0.3)"),
  deleteBg: useColorModeValue("#ffebe9", "rgba(248,81,73,0.1)"),
  deleteGutter: useColorModeValue("#ffd7d5", "rgba(248,81,73,0.3)"),
  number: useColorModeValue("#6e7781", "#6e7681"),
  addStat: useColorModeValue("#1a7f37", "#3fb950"),
  deleteStat: useColorModeValue("#cf222e", "#f85149"),
});

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

const Gutter: React.FC<{
  value?: number | null;
  bg?: string;
  color: string;
}> = ({ value, bg, color }) => (
  <Box
    as="span"
    flex="0 0 auto"
    minW="3.25em"
    px={2}
    textAlign="right"
    color={color}
    bg={bg}
    userSelect="none"
  >
    {value ?? ""}
  </Box>
);

const DiffRow: React.FC<{
  line: ModpackDiffLine;
  colors: DiffColors;
  action?: React.ReactNode;
}> = ({ line, colors, action }) => {
  const bg =
    line.kind === "add"
      ? colors.addBg
      : line.kind === "delete"
        ? colors.deleteBg
        : undefined;
  const gutter =
    line.kind === "add"
      ? colors.addGutter
      : line.kind === "delete"
        ? colors.deleteGutter
        : undefined;
  return (
    <Flex bg={bg} minW="fit-content">
      <Gutter value={line.oldNumber} bg={gutter} color={colors.number} />
      <Gutter value={line.newNumber} bg={gutter} color={colors.number} />
      <Box
        as="span"
        flex="0 0 auto"
        w="1.5em"
        textAlign="center"
        userSelect="none"
      >
        {MARKS[line.kind]}
      </Box>
      <Box as="span" whiteSpace="pre" pr={4}>
        {line.text === "" ? " " : line.text}
      </Box>
      {action && (
        <Box
          as="span"
          ml="auto"
          pl={2}
          pr={2}
          position="sticky"
          right={0}
          flex="0 0 auto"
        >
          {action}
        </Box>
      )}
    </Flex>
  );
};

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

/** The body of a GitHub-style unified diff. */
export const DiffHunks: React.FC<DiffHunksProps> = ({
  hunks,
  hideHunkHeaders = false,
  renderEditAction,
}) => {
  const colors = useDiffColors();
  return (
    <Box
      fontFamily={MONO}
      fontSize="xs"
      lineHeight="20px"
      overflowX="auto"
      role="table"
    >
      {hunks.map((hunk, index) => (
        <React.Fragment key={index}>
          {!hideHunkHeaders && (
            <Flex bg={colors.hunkBg} color={colors.hunkText} minW="fit-content">
              <Box
                as="span"
                flex="0 0 auto"
                minW="calc(6.5em + 2rem + 1.5em)"
                bg={colors.hunkGutter}
              />
              <Box as="span" whiteSpace="pre" px={2}>
                {`@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`}
              </Box>
            </Flex>
          )}
          {hunk.lines.map((line, lineIndex) => {
            const edit =
              line.edit !== undefined && line.edit !== null
                ? hunk.edits?.[line.edit]
                : undefined;
            const first =
              edit !== undefined &&
              hunk.lines[lineIndex - 1]?.edit !== line.edit;
            return (
              <DiffRow
                key={lineIndex}
                line={line}
                colors={colors}
                action={first && renderEditAction?.(edit, hunk)}
              />
            );
          })}
        </React.Fragment>
      ))}
    </Box>
  );
};

interface DiffFileProps {
  path: string;
  /** Shown as `+a −d` next to the path. */
  hunks?: ModpackDiffHunk[] | null;
  /** Badges or notes after the path. */
  tags?: React.ReactNode;
  /** Buttons at the right end of the header. */
  actions?: React.ReactNode;
  defaultOpen?: boolean;
  children?: React.ReactNode;
}

/** A file in a GitHub-style diff: a header bar over a collapsible body. */
export const DiffFile: React.FC<DiffFileProps> = ({
  path,
  hunks,
  tags,
  actions,
  defaultOpen = true,
  children,
}) => {
  const colors = useDiffColors();
  const { isOpen, onToggle } = useDisclosure({ defaultIsOpen: defaultOpen });
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
        <Collapse in={isOpen} animateOpacity={false}>
          {children}
        </Collapse>
      )}
    </Box>
  );
};
