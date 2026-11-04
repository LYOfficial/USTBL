import {
  Box,
  Button,
  ButtonGroup,
  HStack,
  Icon,
  Text,
  VStack,
} from "@chakra-ui/react";
import React, { memo, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuChevronDown, LuChevronRight } from "react-icons/lu";
import {
  DiffExpandSignal,
  DiffFile,
  DiffHunks,
  regionHunk,
} from "@/components/common/diff-view";
import {
  ModpackConflict,
  ModpackConflictChoice,
  ModpackFileChange,
  ModpackMergeConflict,
  ModpackUpdatePlan,
} from "@/models/modpack-update";
import { groupPlanChanges } from "@/utils/modpack-update-groups";

const formatSize = (size?: number) => {
  if (size === undefined) return "";
  if (size < 1024) return `${size} B`;
  const units = ["KiB", "MiB", "GiB"];
  let value = size / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
};

const fileName = (path: string) => path.split("/").pop() ?? path;

/** Which sizes a group's rows show; a name instead of a callback keeps props stable. */
type SizeMode = "restored" | "added" | "updated" | "removed";

const renderSize = (
  mode: SizeMode,
  change: ModpackFileChange,
  t: (key: string) => string
) => {
  switch (mode) {
    case "restored":
      return `${t(`UpdateModpackModal.plan.restoredKind.${change.kind}`)} ${
        change.kind === "add"
          ? formatSize(change.remote?.size)
          : change.kind === "update"
            ? `${formatSize(change.localSize)} → ${formatSize(change.remote?.size)}`
            : formatSize(change.localSize)
      }`;
    case "added":
      return formatSize(change.remote?.size);
    case "updated":
      return `${formatSize(change.localSize)} → ${formatSize(change.remote?.size)}`;
    case "removed":
      return formatSize(change.localSize);
  }
};

/** One collapsible group of file changes, capped until "show all". */
const ChangeGroup: React.FC<{
  title: string;
  colorScheme: string;
  changes: ModpackFileChange[];
  sizeMode: SizeMode;
  description?: string;
  defaultOpen?: boolean;
}> = ({
  title,
  colorScheme,
  changes,
  sizeMode,
  description,
  defaultOpen = false,
}) => {
  const { t } = useTranslation();
  const [open, setOpen] = useState(defaultOpen);
  const [showAll, setShowAll] = useState(false);
  // Sorting thousands of paths is only worth it for a group that is open.
  const sorted = useMemo(
    () =>
      open ? [...changes].sort((a, b) => a.path.localeCompare(b.path)) : [],
    [open, changes]
  );
  if (changes.length === 0) return null;
  const shown = showAll ? sorted : sorted.slice(0, 100);
  return (
    <Box borderWidth="1px" borderRadius="md">
      <HStack
        as="button"
        type="button"
        w="full"
        px={3}
        py={2}
        spacing={2}
        onClick={() => setOpen(!open)}
        aria-expanded={open}
      >
        <Icon as={open ? LuChevronDown : LuChevronRight} />
        <Text fontWeight="medium" color={`${colorScheme}.500`}>
          {t(title, { count: changes.length })}
        </Text>
      </HStack>
      {open && (
        <VStack align="stretch" spacing={0} borderTopWidth="1px">
          {description && (
            <Text fontSize="xs" className="secondary-text" px={3} py={1.5}>
              {description}
            </Text>
          )}
          {shown.map((change) => (
            <HStack
              key={change.path}
              justify="space-between"
              spacing={3}
              px={3}
              py={1.5}
              borderTopWidth="1px"
            >
              <Box minW={0}>
                <Text fontSize="sm" noOfLines={1} wordBreak="break-all">
                  {fileName(change.path)}
                </Text>
                {change.path.includes("/") && (
                  <Text fontSize="xs" className="secondary-text" noOfLines={1}>
                    {change.path}
                  </Text>
                )}
              </Box>
              <Text fontSize="xs" className="secondary-text" flexShrink={0}>
                {renderSize(sizeMode, change, t)}
              </Text>
            </HStack>
          ))}
          {!showAll && sorted.length > shown.length && (
            <Button
              variant="ghost"
              size="xs"
              onClick={() => setShowAll(true)}
              borderTopWidth="1px"
              borderRadius={0}
            >
              {t("UpdateModpackModal.plan.showAll", {
                count: sorted.length - shown.length,
              })}
            </Button>
          )}
        </VStack>
      )}
    </Box>
  );
};

/**
 * Every group of file changes of a plan. Memoized on the plan so answering
 * conflicts does not regroup and re-render the lists.
 */
export const PlanChangeGroups = memo(function PlanChangeGroups({
  plan,
}: {
  plan: ModpackUpdatePlan;
}) {
  const { t } = useTranslation();
  const groups = useMemo(() => groupPlanChanges(plan), [plan]);
  return (
    <VStack align="stretch" spacing={2}>
      <ChangeGroup
        title="UpdateModpackModal.plan.restored"
        colorScheme="orange"
        changes={groups.restored}
        description={t("UpdateModpackModal.plan.restoredDescription")}
        defaultOpen
        sizeMode="restored"
      />
      <ChangeGroup
        title="UpdateModpackModal.plan.add"
        colorScheme="green"
        changes={groups.add}
        sizeMode="added"
      />
      <ChangeGroup
        title="UpdateModpackModal.plan.update"
        colorScheme="blue"
        changes={groups.update}
        sizeMode="updated"
      />
      <ChangeGroup
        title="UpdateModpackModal.plan.delete"
        colorScheme="red"
        changes={groups.delete}
        sizeMode="removed"
      />
      <ChangeGroup
        title="UpdateModpackModal.plan.disable"
        colorScheme="gray"
        changes={groups.disable}
        sizeMode="removed"
      />
      <ChangeGroup
        title="UpdateModpackModal.plan.enable"
        colorScheme="gray"
        changes={groups.enable}
        sizeMode="removed"
      />
      <ChangeGroup
        title="UpdateModpackModal.plan.retire"
        colorScheme="gray"
        changes={groups.retire}
        sizeMode="removed"
      />
      <ChangeGroup
        title="UpdateModpackModal.plan.autoMerged"
        colorScheme="green"
        changes={groups.autoMerged}
        sizeMode="updated"
      />
    </VStack>
  );
});

export type ChooseConflict = (
  key: string,
  choice: ModpackConflictChoice
) => void;

const hunkKey = (path: string, index: number) => `${path}#${index}`;

/** Answers every key at once: all blocks of one file. */
export type ChooseConflictKeys = (
  keys: string[],
  choice: ModpackConflictChoice
) => void;

/** The two answer buttons; `choice` null highlights neither (mixed answers). */
const ChoiceToggle: React.FC<{
  choice: ModpackConflictChoice | null;
  primaryColor: string;
  onPick: (choice: ModpackConflictChoice) => void;
}> = ({ choice, primaryColor, onPick }) => {
  const { t } = useTranslation();
  return (
    <ButtonGroup size="xs" isAttached variant="outline" flexShrink={0}>
      {(["keepLocal", "takeRemote"] as const).map((option) => (
        <Button
          key={option}
          colorScheme={choice === option ? primaryColor : undefined}
          variant={choice === option ? "solid" : "outline"}
          aria-pressed={choice === option}
          onClick={() => onPick(option)}
        >
          {t(`UpdateModpackModal.choice.${option}`)}
        </Button>
      ))}
    </ButtonGroup>
  );
};

const ChoiceButtons = memo(function ChoiceButtons({
  choiceKey,
  choice,
  primaryColor,
  onChoose,
}: {
  choiceKey: string;
  choice: ModpackConflictChoice;
  primaryColor: string;
  onChoose: ChooseConflict;
}) {
  return (
    <ChoiceToggle
      choice={choice}
      primaryColor={primaryColor}
      onPick={(option) => onChoose(choiceKey, option)}
    />
  );
});

/** One side of a block. Its diff depends on the content only, never on the answer. */
const HunkSide = memo(function HunkSide({
  hunk,
  side,
}: {
  hunk: ModpackMergeConflict;
  side: "keepLocal" | "takeRemote";
}) {
  const { t } = useTranslation();
  const hunks = useMemo(
    () => [
      regionHunk(
        hunk.baseStart + 1,
        hunk.base,
        side === "keepLocal" ? hunk.local : hunk.remote
      ),
    ],
    [hunk, side]
  );
  return (
    <>
      <Text fontSize="xs" fontWeight="medium" px={3} pt={1}>
        {t(
          side === "keepLocal"
            ? "UpdateModpackModal.choice.localSide"
            : "UpdateModpackModal.choice.remoteSide"
        )}
      </Text>
      <DiffHunks hideHunkHeaders hunks={hunks} />
    </>
  );
});

const ConflictHunk = memo(function ConflictHunk({
  path,
  index,
  hunk,
  choice,
  primaryColor,
  onChoose,
}: {
  path: string;
  index: number;
  hunk: ModpackMergeConflict;
  choice: ModpackConflictChoice;
  primaryColor: string;
  onChoose: ChooseConflict;
}) {
  const { t } = useTranslation();
  const key = hunkKey(path, index);
  return (
    <VStack align="stretch" spacing={0} borderTopWidth={index > 0 ? "1px" : 0}>
      <HStack justify="space-between" px={3} py={1.5} spacing={2}>
        <Text fontSize="xs" className="secondary-text" minW={0}>
          {t("UpdateModpackModal.choice.hunk", {
            number: index + 1,
            from: hunk.baseStart + 1,
            to: Math.max(hunk.baseEnd, hunk.baseStart + 1),
          })}
        </Text>
        <ChoiceButtons
          choiceKey={key}
          choice={choice}
          primaryColor={primaryColor}
          onChoose={onChoose}
        />
      </HStack>
      {(["keepLocal", "takeRemote"] as const).map((side) => (
        <Box key={side} opacity={choice === side ? 1 : 0.55}>
          <HunkSide hunk={hunk} side={side} />
        </Box>
      ))}
    </VStack>
  );
});

interface ConflictItemProps {
  conflict: ModpackConflict;
  choices: Record<string, ModpackConflictChoice>;
  primaryColor: string;
  onChoose: ChooseConflict;
  onChooseMany: ChooseConflictKeys;
  onReveal: (path: string) => void;
  expandSignal?: DiffExpandSignal;
}

const choiceOf = (
  choices: Record<string, ModpackConflictChoice>,
  key: string
): ModpackConflictChoice => choices[key] ?? "keepLocal";

/**
 * Re-render a conflict only when the conflict itself or one of its own
 * answers changed; the choices object is new on every click.
 */
export const conflictItemEqual = (
  prev: ConflictItemProps,
  next: ConflictItemProps
) => {
  if (
    prev.conflict !== next.conflict ||
    prev.primaryColor !== next.primaryColor ||
    prev.onChoose !== next.onChoose ||
    prev.onChooseMany !== next.onChooseMany ||
    prev.onReveal !== next.onReveal ||
    prev.expandSignal !== next.expandSignal
  ) {
    return false;
  }
  if (prev.choices === next.choices) return true;
  const { path, hunks } = next.conflict;
  if (hunks.length === 0) {
    return choiceOf(prev.choices, path) === choiceOf(next.choices, path);
  }
  return hunks.every((_, index) => {
    const key = hunkKey(path, index);
    return choiceOf(prev.choices, key) === choiceOf(next.choices, key);
  });
};

export const ConflictItem = memo(function ConflictItem({
  conflict,
  choices,
  primaryColor,
  onChoose,
  onChooseMany,
  onReveal,
  expandSignal,
}: ConflictItemProps) {
  const { t } = useTranslation();
  const revealButton = (
    <Button size="xs" variant="ghost" onClick={() => onReveal(conflict.path)}>
      {t("UpdateModpackModal.choice.revealFile")}
    </Button>
  );
  const kindText = (
    <Text fontSize="xs" className="secondary-text" noOfLines={1}>
      {t(`UpdateModpackModal.conflictKind.${conflict.kind}`)}
    </Text>
  );
  // A conflict without text blocks (a binary or replaced file) has nothing to
  // expand, so it is one row answered as a whole file.
  if (conflict.hunks.length === 0) {
    return (
      <HStack
        borderWidth="1px"
        borderRadius="md"
        px={2}
        py={1.5}
        spacing={2}
        justify="space-between"
      >
        <VStack align="stretch" spacing={0} minW={0}>
          <Text
            fontFamily="mono"
            fontSize="xs"
            fontWeight="semibold"
            noOfLines={1}
            wordBreak="break-all"
          >
            {conflict.path}
          </Text>
          {kindText}
        </VStack>
        <HStack spacing={1} flexShrink={0}>
          {revealButton}
          <ChoiceButtons
            choiceKey={conflict.path}
            choice={choiceOf(choices, conflict.path)}
            primaryColor={primaryColor}
            onChoose={onChoose}
          />
        </HStack>
      </HStack>
    );
  }
  // A text conflict is answered block by block, or for all blocks of the file
  // at once; the file buttons show an answer only when every block has it.
  const keys = conflict.hunks.map((_, index) => hunkKey(conflict.path, index));
  const first = choiceOf(choices, keys[0]);
  const fileChoice = keys.every((key) => choiceOf(choices, key) === first)
    ? first
    : null;
  return (
    <DiffFile
      path={conflict.path}
      expandSignal={expandSignal}
      tags={kindText}
      actions={
        <>
          {revealButton}
          <ChoiceToggle
            choice={fileChoice}
            primaryColor={primaryColor}
            onPick={(option) => onChooseMany(keys, option)}
          />
        </>
      }
    >
      <VStack align="stretch" spacing={0}>
        {conflict.hunks.map((hunk, index) => (
          <ConflictHunk
            key={keys[index]}
            path={conflict.path}
            index={index}
            hunk={hunk}
            choice={choiceOf(choices, keys[index])}
            primaryColor={primaryColor}
            onChoose={onChoose}
          />
        ))}
      </VStack>
    </DiffFile>
  );
}, conflictItemEqual);
