import {
  Button,
  Flex,
  HStack,
  Icon,
  IconButton,
  Tag,
  TagLabel,
  Tooltip,
} from "@chakra-ui/react";
import { useTranslation } from "react-i18next";
import { LuFoldVertical, LuUnfoldVertical } from "react-icons/lu";
import Empty from "@/components/common/empty";
import FileFilterInput from "@/components/common/file-filter-input";
import {
  CHANGE_COLOR,
  ChangeVisual,
  changeLabelKey,
} from "@/utils/modpack-change-style";

interface FilePanelToolbarProps {
  query: string;
  onQueryChange: (query: string) => void;
  /** Type buttons and switches; they sit at the right and wrap with the rest. */
  controls?: React.ReactNode;
  /** The two icon buttons show only for the handlers given. */
  onExpandAll?: () => void;
  onCollapseAll?: () => void;
}

/**
 * The bar over a file list: the filter box at the left, the type buttons and
 * the expand and collapse buttons at the right. A narrow bar wraps.
 */
export const FilePanelToolbar: React.FC<FilePanelToolbarProps> = ({
  query,
  onQueryChange,
  controls,
  onExpandAll,
  onCollapseAll,
}) => {
  const { t } = useTranslation();
  const toggles = (
    [
      [onExpandAll, "expandAll", LuUnfoldVertical],
      [onCollapseAll, "collapseAll", LuFoldVertical],
    ] as const
  ).filter(([onClick]) => onClick);
  return (
    <Flex align="center" wrap="wrap" gap={2} w="100%">
      <FileFilterInput w="180px" value={query} onChange={onQueryChange} />
      <HStack ml="auto" spacing={2} flexWrap="wrap" justify="flex-end">
        {controls}
        {toggles.length > 0 && (
          <HStack spacing={1} flexShrink={0}>
            {toggles.map(([onClick, key, icon]) => (
              <Tooltip key={key} label={t(`FilePanel.${key}`)}>
                <IconButton
                  size="xs"
                  variant="ghost"
                  aria-label={t(`FilePanel.${key}`)}
                  icon={<Icon as={icon} />}
                  onClick={onClick}
                />
              </Tooltip>
            ))}
          </HStack>
        )}
      </HStack>
    </Flex>
  );
};

/** A directory or group button that opens or closes its whole subtree. */
export const RecursiveToggle: React.FC<{
  open: boolean;
  onOpenChange: (open: boolean) => void;
}> = ({ open, onOpenChange }) => {
  const { t } = useTranslation();
  const label = t(`FilePanel.${open ? "collapseSubtree" : "expandSubtree"}`);
  return (
    <Tooltip label={label}>
      <IconButton
        size="xs"
        variant="ghost"
        flexShrink={0}
        opacity={0}
        _groupHover={{ opacity: 1 }}
        _groupFocusWithin={{ opacity: 1 }}
        _focusVisible={{ opacity: 1 }}
        aria-label={label}
        aria-expanded={open}
        icon={<Icon as={open ? LuFoldVertical : LuUnfoldVertical} />}
        onClick={() => onOpenChange(!open)}
      />
    </Tooltip>
  );
};

/** The colored label of a kind of change. */
export const ChangeTag: React.FC<{ visual: ChangeVisual }> = ({ visual }) => {
  const { t } = useTranslation();
  return (
    <Tag
      size="sm"
      variant="subtle"
      colorScheme={CHANGE_COLOR[visual]}
      flexShrink={0}
    >
      <TagLabel>{t(changeLabelKey(visual))}</TagLabel>
    </Tag>
  );
};

interface ChangeFilterButtonProps {
  visual: ChangeVisual;
  count: number;
  selected: boolean;
  onClick: () => void;
  isDisabled?: boolean;
}

/** A button that filters by one kind of change; the count follows the label. */
export const ChangeFilterButton: React.FC<ChangeFilterButtonProps> = ({
  visual,
  count,
  selected,
  onClick,
  isDisabled,
}) => {
  const { t } = useTranslation();
  return (
    <Button
      size="xs"
      colorScheme={CHANGE_COLOR[visual]}
      variant={selected ? "solid" : "outline"}
      isDisabled={isDisabled}
      aria-pressed={selected}
      onClick={onClick}
    >
      {t(changeLabelKey(visual))} {count}
    </Button>
  );
};

/** What a file list shows when nothing is in it; `filtered` when a filter hides everything. */
export const FilePanelEmpty: React.FC<{ filtered?: boolean }> = ({
  filtered = false,
}) => {
  const { t } = useTranslation();
  return (
    <Empty
      withIcon={false}
      size="sm"
      description={filtered ? t("FileFilter.empty") : undefined}
    />
  );
};
