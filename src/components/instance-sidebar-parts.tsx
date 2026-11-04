import {
  Box,
  Button,
  HStack,
  Icon,
  Input,
  Text,
  Tooltip,
  VStack,
} from "@chakra-ui/react";
import React, { useCallback, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { IconType } from "react-icons";
import { FaStar } from "react-icons/fa6";
import { LuBox, LuChevronDown, LuChevronLeft } from "react-icons/lu";
import SelectableButton from "@/components/common/selectable-button";
import { InstanceSummary } from "@/models/instance/misc";
import { getGameDirName } from "@/utils/instance";

export const DOUBLE_CLICK_DELAY_MS = 250;
export const INSTANCE_TOOLTIP_DELAY_MS = 2000;

/**
 * Runs `onSingle` only once it is clear that no second click follows, and
 * `onDouble` immediately on the second click.
 */
export const useClickOrDoubleClick = (
  onSingle: () => void,
  onDouble: () => void,
  delay = DOUBLE_CLICK_DELAY_MS
) => {
  const timer = useRef<number>();
  useEffect(() => () => window.clearTimeout(timer.current), []);
  return useCallback(
    (event: React.MouseEvent) => {
      window.clearTimeout(timer.current);
      if (event.detail >= 2) onDouble();
      else timer.current = window.setTimeout(onSingle, delay);
    },
    [delay, onDouble, onSingle]
  );
};

interface InlineNameInputProps {
  initial: string;
  onSubmit: (value: string) => void;
  onCancel: () => void;
}

export const InlineNameInput: React.FC<InlineNameInputProps> = ({
  initial,
  onSubmit,
  onCancel,
}) => {
  const finished = useRef(false);
  const finish = (action: () => void) => {
    if (finished.current) return;
    finished.current = true;
    action();
  };
  return (
    <Input
      size="xs"
      autoFocus
      defaultValue={initial}
      onFocus={(event) => event.target.select()}
      onClick={(event) => event.stopPropagation()}
      onPointerDown={(event) => event.stopPropagation()}
      onKeyDown={(event) => {
        event.stopPropagation();
        if (event.nativeEvent.isComposing) return;
        if (event.key === "Enter") {
          finish(() => onSubmit(event.currentTarget.value));
        } else if (event.key === "Escape") {
          finish(onCancel);
        }
      }}
      onBlur={(event) => finish(() => onSubmit(event.target.value))}
    />
  );
};

interface InstanceRowProps {
  instance: InstanceSummary;
  selected: boolean;
  editing: boolean;
  tooltipDisabled?: boolean;
  onNavigate: () => void;
  onStartEdit: () => void;
  onSubmitName: (name: string) => void;
  onCancelEdit: () => void;
  onPointerDown?: (event: React.PointerEvent) => void;
}

export const InstanceRow: React.FC<InstanceRowProps> = ({
  instance,
  selected,
  editing,
  tooltipDisabled,
  onNavigate,
  onStartEdit,
  onSubmitName,
  onCancelEdit,
  onPointerDown,
}) => {
  const { t } = useTranslation();
  const handleClick = useClickOrDoubleClick(onNavigate, onStartEdit);
  const icon = instance.starred ? FaStar : LuBox;

  if (editing) {
    return (
      <HStack spacing={2} h={8} px={3}>
        <Icon as={icon} />
        <InlineNameInput
          initial={instance.name}
          onSubmit={onSubmitName}
          onCancel={onCancelEdit}
        />
      </HStack>
    );
  }

  const location =
    instance.gameDir && instance.folder
      ? t("InstanceGroups.tooltip.location", {
          gameDir: getGameDirName(instance.gameDir),
          folder: instance.folder,
        })
      : undefined;

  return (
    <Tooltip
      label={
        <VStack align="start" spacing={0}>
          <Text>{instance.name}</Text>
          {location && <Text fontSize="xs">{location}</Text>}
        </VStack>
      }
      placement="right"
      openDelay={INSTANCE_TOOLTIP_DELAY_MS}
      closeOnPointerDown
      isDisabled={tooltipDisabled}
    >
      {/* SelectableButton does not forward refs, so the tooltip anchors here */}
      <Box>
        <SelectableButton
          size="sm"
          w="100%"
          isSelected={selected}
          onClick={handleClick}
          onPointerDown={onPointerDown}
          data-instance-id={instance.id}
        >
          <HStack spacing={2} overflow="hidden">
            <Icon as={icon} />
            <Text fontSize="sm" className="ellipsis-text">
              {instance.name}
            </Text>
          </HStack>
        </SelectableButton>
      </Box>
    </Tooltip>
  );
};

interface SectionHeaderProps {
  title: string;
  count: number;
  icon: IconType;
  open: boolean;
  editing?: boolean;
  onToggle: () => void;
  onStartEdit?: () => void;
  onSubmitName?: (name: string) => void;
  onCancelEdit?: () => void;
  onPointerDown?: (event: React.PointerEvent) => void;
  onContextMenu?: (event: React.MouseEvent) => void;
}

export const SectionHeader: React.FC<SectionHeaderProps> = ({
  title,
  count,
  icon,
  open,
  editing,
  onToggle,
  onStartEdit,
  onSubmitName,
  onCancelEdit,
  onPointerDown,
  onContextMenu,
}) => {
  // Only renamable headers wait for a possible second click.
  const handleClick = useClickOrDoubleClick(onToggle, onStartEdit ?? onToggle);
  const chevron = <Icon as={open ? LuChevronDown : LuChevronLeft} />;

  if (editing && onSubmitName && onCancelEdit) {
    return (
      <HStack spacing={2} h={8} px={3}>
        <Icon as={icon} />
        <InlineNameInput
          initial={title}
          onSubmit={onSubmitName}
          onCancel={onCancelEdit}
        />
        {chevron}
      </HStack>
    );
  }

  return (
    <Button
      variant="ghost"
      size="sm"
      w="100%"
      justifyContent="start"
      onClick={onStartEdit ? handleClick : onToggle}
      onPointerDown={onPointerDown}
      onContextMenu={onContextMenu}
      aria-expanded={open}
      rightIcon={chevron}
    >
      <HStack spacing={2} overflow="hidden" flex={1} minW={0}>
        <Icon as={icon} />
        <Text className="ellipsis-text">{title}</Text>
        <Text className="secondary-text" fontSize="xs">
          {count}
        </Text>
      </HStack>
    </Button>
  );
};
